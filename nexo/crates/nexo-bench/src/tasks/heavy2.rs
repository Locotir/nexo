//! Heavy tasks for a context curator: the work happens in a debugger, in a dump of HTTP traffic or in
//! an assembly listing, where one command can print thousands of lines and the answer rests on a few
//! of them. Nine tasks, group `heavy2`: four binary-debugging jobs (`dbg-core`, `dbg-heap`,
//! `dbg-corrupt`, `dbg-uaf`), three HTTP jobs (`http-trace`, `http-api-diff`, `http-har-leak`) and two
//! assembly jobs (`asm-fix`, `asm-read`).
//!
//! Nothing here calls a model or reaches the network: the HTTP responder listens on `127.0.0.1`, the
//! programs are built from sources that never enter the working directory, and every checker works from
//! the generator's own facts.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::Task;
use crate::check::{run, Output};

mod asm;
mod dbg;
mod http;

pub fn tasks() -> Vec<Task> {
    let mut out = dbg::tasks();
    out.extend(http::tasks());
    out.extend(asm::tasks());
    out
}

/// Ok when every program in `tools` is on the `PATH`.
pub(crate) fn have(tools: &[&str]) -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for t in tools {
        let found = std::env::split_paths(&path).any(|d| {
            let p = d.join(t);
            p.is_file() || p.with_extension("exe").is_file()
        });
        if !found {
            return Err(format!("this task needs `{t}` on the PATH"));
        }
    }
    Ok(())
}

/// Compiles `src` with `cc` into `out`; the source stays in a folder of its own.
pub(crate) fn compile_c(src: &str, out: &Path, flags: &[&str]) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let file = tmp.path().join("p.c");
    std::fs::write(&file, src).map_err(|e| e.to_string())?;
    let got = run(Command::new("cc").args(flags).arg("-o").arg(out).arg(&file), None, Duration::from_secs(120));
    if got.success() {
        Ok(())
    } else {
        Err(format!("cc failed: {}", got.tail(400)))
    }
}

/// Compiles `src` with `rustc` into `out`; the source stays in a folder of its own.
pub(crate) fn compile_rs(src: &Path, out: &Path) -> Output {
    run(
        Command::new("rustc").arg("-O").arg("--edition=2021").arg("-o").arg(out).arg(src),
        None,
        Duration::from_secs(180),
    )
}

/// The `key: value` lines of an answer file, keys lower-cased; empty when the file is missing.
pub(crate) fn keyed(wd: &Path, file: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(wd.join(file)).unwrap_or_default();
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}

/// One graded line: the answer file holds `want` under `key`.
pub(crate) fn line(got: &BTreeMap<String, String>, key: &str, want: &str) -> (bool, String) {
    let have = got.get(key).map(String::as_str).unwrap_or("(missing)");
    (have == want, format!("{key}: {have:?}, expected {want:?}"))
}

/// Runs `gdb` on `bin` (built in a folder of its own with `input` as `input_name`) until it stops on a
/// signal, then saves the core file; `bin` and `core` end up in `wd`.
pub(crate) fn crash_core(wd: &Path, bin_name: &str, src: &str, input_name: &str, input: &str) -> Result<(), String> {
    have(&["cc", "gdb"])?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let bin = tmp.path().join(bin_name);
    compile_c(src, &bin, &["-O1", "-s", "-fno-stack-protector", "-U_FORTIFY_SOURCE"])?;
    std::fs::write(tmp.path().join(input_name), input).map_err(|e| e.to_string())?;
    let got = run(
        Command::new("gdb")
            .current_dir(tmp.path())
            .args(["-batch", "-nx", "-ex", "run", "-ex", "generate-core-file core", "--args"])
            .arg(format!("./{bin_name}"))
            .arg(input_name),
        None,
        Duration::from_secs(120),
    );
    let core = tmp.path().join("core");
    if !core.is_file() {
        return Err(format!("gdb left no core file: {}", got.tail(400)));
    }
    std::fs::copy(&bin, wd.join(bin_name)).map_err(|e| e.to_string())?;
    std::fs::copy(&core, wd.join("core")).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every checker against its own solution, and against the untouched start.
    #[test]
    fn every_heavy2_checker_accepts_its_solution_and_rejects_the_start() {
        let mut bad = Vec::new();
        for task in tasks() {
            if let Err(e) = (task.ready)() {
                eprintln!("{} skipped: {e}", task.id);
                continue;
            }
            let wd = tempfile::tempdir().unwrap();
            if let Err(e) = task.materialize(wd.path()) {
                bad.push(format!("{}: setup failed: {e}", task.id));
                continue;
            }
            if (task.check)(wd.path()).ok {
                bad.push(format!("{}: the untouched task passes", task.id));
            }
            (task.solution)(wd.path());
            let got = (task.check)(wd.path());
            if !got.ok || got.score != 1.0 {
                bad.push(format!("{}: {}", task.id, got.detail));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn the_nine_tasks_have_distinct_ids_and_belong_to_heavy2() {
        let t = tasks();
        assert_eq!(t.len(), 9);
        assert!(t.iter().all(|t| t.group == "heavy2"));
        let mut ids: Vec<&str> = t.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 9);
        for id in [
            "dbg-core",
            "dbg-heap",
            "dbg-corrupt",
            "dbg-uaf",
            "http-trace",
            "http-api-diff",
            "http-har-leak",
            "asm-fix",
            "asm-read",
        ] {
            assert!(ids.contains(&id), "{id}");
        }
    }
}
