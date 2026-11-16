#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A library the program loads itself (`dlopen`) after the start: a breakpoint or hook on a name only that library
//! has waits for it, `catch load` and `catch unload` stop when it comes and goes, and `ltrace` takes its own import
//! calls. The program and the library are compiled here; skipped without `cc`.

use nexo_dbg::{execute, parse, Host};

/// The library: one function that calls into libc, so its own import is something to trace.
const PLUGIN: &str = r#"
#include <stdlib.h>
__attribute__((noinline)) int plugin_fn(const char *s) { return atoi(s) + 1; }
"#;

/// Loads the library named in argv[1], calls `plugin_fn` in it, unloads it.
const HOST: &str = r#"
#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    void *h = dlopen(argv[1], RTLD_NOW);
    if (!h) { fprintf(stderr, "%s\n", dlerror()); return 3; }
    int (*f)(const char *) = (int (*)(const char *))dlsym(h, "plugin_fn");
    int r = f("41");
    printf("plugin %d\n", r);
    fflush(stdout);
    dlclose(h);
    return r == 42 ? 0 : 1;
}
"#;

/// The host program and the library it loads, built with `flags` (`-m32` for a 32-bit pair).
fn build(dir: &std::path::Path, flags: &[&str]) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let cc = |out: &std::path::Path, src: &str, name: &str, more: &[&str]| {
        let c = dir.join(name);
        std::fs::write(&c, src).unwrap();
        std::process::Command::new("cc")
            .args(["-O0", "-g"])
            .args(flags)
            .args(more)
            .arg("-o")
            .arg(out)
            .arg(&c)
            .args(if more.is_empty() { &["-ldl"][..] } else { &[][..] })
            .status()
            .is_ok_and(|s| s.success())
    };
    let lib = dir.join("libplugin.so");
    let exe = dir.join("host");
    if !cc(&lib, PLUGIN, "plugin.c", &["-shared", "-fPIC"]) || !cc(&exe, HOST, "host.c", &[]) {
        eprintln!("SKIPPED: cc {flags:?} cannot build the program and its library here, so nothing was checked");
        return None;
    }
    Some((exe, lib))
}

fn call(state: &std::path::Path, words: &[&str]) -> String {
    let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
    match parse(&w).and_then(|c| execute(&c, state, &Host::Thread, "nexo dbg")) {
        Ok(s) => s,
        Err(e) => format!("error: {e}"),
    }
}

/// A breakpoint set at the entry on a function only the library has is hit inside the library once it is loaded.
fn pending_breakpoint_is_hit(flags: &[&str]) {
    let work = tempfile::tempdir().unwrap();
    let Some((exe, lib)) = build(work.path(), flags) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), lib.to_str().unwrap()]);
    let b = call(s, &["break", "plugin_fn"]);
    assert!(b.contains("breakpoint #1 pending on plugin_fn") && b.contains("no loaded file has it"), "{b}");
    let l = call(s, &["breaks"]);
    assert!(l.contains("plugin_fn (pending"), "{l}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("breakpoint #1"), "{out}");
    assert!(out.contains("libplugin.so!plugin_fn"), "{out}");
    let l = call(s, &["breaks"]);
    assert!(l.contains("libplugin.so!plugin_fn hits 1"), "{l}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_breakpoint_set_before_dlopen_on_a_name_only_the_library_has_is_hit_in_that_library() {
    pending_breakpoint_is_hit(&[]);
}

#[test]
fn a_breakpoint_set_before_dlopen_is_hit_in_a_32_bit_program_too() {
    pending_breakpoint_is_hit(&["-m32"]);
}

#[test]
fn catch_load_and_unload_stop_when_the_library_comes_and_goes_and_its_breakpoint_waits_again() {
    let work = tempfile::tempdir().unwrap();
    let Some((exe, lib)) = build(work.path(), &[]) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), lib.to_str().unwrap()]);
    let c = call(s, &["catch", "load", "plugin"]);
    assert!(c.contains("stops when a library whose file name holds \"plugin\" loads"), "{c}");
    let c = call(s, &["catch", "unload"]);
    assert!(c.contains("stops when any library is unloaded"), "{c}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("library loaded: libplugin.so at 0x"), "{out}");
    // loaded now: the name is found at once
    let b = call(s, &["break", "plugin_fn"]);
    assert!(b.contains("breakpoint #1 at 0x") && b.contains("libplugin.so!plugin_fn"), "{b}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("breakpoint #1"), "{out}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("library unloaded: libplugin.so"), "{out}");
    let l = call(s, &["breaks"]);
    assert!(l.contains("plugin_fn (pending"), "{l}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    let _ = call(s, &["end"]);
}

#[test]
fn ltrace_traces_an_import_only_a_library_loaded_later_calls() {
    let work = tempfile::tempdir().unwrap();
    let Some((exe, lib)) = build(work.path(), &[]) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), lib.to_str().unwrap()]);
    let t = call(s, &["ltrace", "atoi"]);
    assert!(t.contains("tracing library calls: atoi") && t.contains("a library the program loads from now on"), "{t}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    let shown = call(s, &["ltrace", "show"]);
    assert!(shown.contains("atoi(") && shown.contains("\"41\"") && shown.contains("= 41"), "{shown}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_hook_on_a_name_only_a_later_library_has_waits_for_it_and_sees_its_call() {
    let work = tempfile::tempdir().unwrap();
    let Some((exe, lib)) = build(work.path(), &[]) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), lib.to_str().unwrap()]);
    let h = call(s, &["hook", "plugin_fn"]);
    assert!(h.contains("hook #1 pending on plugin_fn"), "{h}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    let l = call(s, &["hooks"]);
    assert!(l.contains("plugin_fn entries 1 returns 1"), "{l}");
    let _ = call(s, &["end"]);
}

#[test]
fn a_name_no_file_has_and_no_library_brings_never_resolves_and_the_end_says_so() {
    let work = tempfile::tempdir().unwrap();
    let Some((exe, lib)) = build(work.path(), &[]) else { return };
    let state = tempfile::tempdir().unwrap();
    let s = state.path();
    call(s, &["run", exe.to_str().unwrap(), lib.to_str().unwrap()]);
    let b = call(s, &["break", "no_such_fn"]);
    assert!(b.contains("pending on no_such_fn"), "{b}");
    let out = call(s, &["cont", "--wait", "20"]);
    assert!(out.contains("exited with code 0"), "{out}");
    assert!(out.contains("breakpoint #1 pending on no_such_fn was never set"), "{out}");
    let _ = call(s, &["end"]);
}
