#![cfg(all(windows, target_arch = "x86_64"))]
//! The `debug` tool on Windows x86-64, against a real Windows program (nexo-dbg's examples/dbg_target.rs): it
//! starts at the entry, stops at a breakpoint, and says what is true about what Windows cannot do. The program is
//! built by cargo as an example of nexo-dbg (`cargo build -p nexo-dbg --example dbg_target`) when it is not there.
//! A program whose function names live only in a .pdb that cannot be read skips the breakpoint step, with a message.

use std::path::PathBuf;

use nexo_core::{Tool, ToolContext, ToolOutput};
use nexo_tools::debug::Debug;
use serde_json::json;

fn fixture() -> PathBuf {
    let name = format!("dbg_target{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().unwrap();
    if let Some(t) = exe.ancestors().skip(1).take(6).map(|d| d.join("examples").join(&name)).find(|t| t.is_file()) {
        return t;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["build", "-q", "-p", "nexo-dbg", "--example", "dbg_target", "--message-format=json"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo runs");
    assert!(out.status.success(), "dbg_target did not build: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["target"]["name"] == "dbg_target")
        .find_map(|v| v["executable"].as_str().map(PathBuf::from))
        .expect("cargo names the example it built")
}

async fn call(t: &Debug, cmd: &str) -> ToolOutput {
    t.run(json!({"command": cmd}), &ToolContext::new(std::env::temp_dir())).await
}

#[test]
fn the_text_names_windows_and_leaves_out_system_call_tracing() {
    let d = Debug::new().spec().description;
    assert!(d.starts_with("Debugger for programs you run (Windows x86-64)."), "{d}");
    assert!(!d.contains("strace") && !d.contains("catch syscall"), "{d}");
    assert!(d.contains("ltrace [names]") && d.contains("break <addr|name|fn+off>"), "{d}");
    assert!(!Debug::new().compact().unwrap().description.contains("strace"));
}

#[tokio::test]
async fn a_program_runs_to_a_breakpoint_through_the_tool_and_syscall_tracing_is_refused_truthfully() {
    let exe = fixture();
    let dir = tempfile::tempdir().unwrap();
    let t = Debug::in_dir(dir.path());
    let first = call(&t, &format!("run {} loop", exe.display())).await;
    assert!(!first.is_error, "{}", first.content);
    assert!(
        first.content.contains("started pid") && first.content.contains("stopped at the program's entry"),
        "{}",
        first.content
    );
    let regs = call(&t, "regs").await;
    assert!(!regs.is_error && regs.content.contains("rip") && regs.content.contains("rsp"), "{}", regs.content);

    // what Windows has no way to do is said as a fact, with the way to the same answer
    for c in ["strace", "strace show", "catch syscall write"] {
        let o = call(&t, c).await;
        let what = if c.starts_with("catch") { "catch syscall" } else { "strace" };
        assert!(o.is_error, "{c}: {}", o.content);
        assert!(
            o.content.contains(&format!(
                "{what}: system call tracing is not available on Windows; break on the API instead"
            )),
            "{c}: {}",
            o.content
        );
    }

    let b = call(&t, "break dbg_add").await;
    if b.is_error {
        eprintln!(
            "skipped the breakpoint: the example carries no function names the debugger can read ({})",
            b.content
        );
        call(&t, "kill").await;
        return;
    }
    assert!(b.content.contains("breakpoint #1") && b.content.contains("dbg_add"), "{}", b.content);
    let stop = call(&t, "cont --wait 20").await;
    assert!(!stop.is_error, "{}", stop.content);
    assert!(stop.content.contains("breakpoint #1") && stop.content.contains("dbg_add"), "{}", stop.content);
    // the Windows calling convention: the first two arguments are in rcx and rdx
    let args = call(&t, "args").await;
    assert!(args.content.contains("arg1 rcx = 0x0") && args.content.contains("arg2 rdx = 0x1"), "{}", args.content);
    call(&t, "delete 1").await;
    let end = call(&t, "cont --wait 20").await;
    assert!(end.content.contains("exited with code 0"), "{}", end.content);
    call(&t, "end").await;
}
