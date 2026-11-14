#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
//! A core file read back as a stopped program. The core is written here from a live crash (the kernel's own
//! cores depend on the machine's `core_pattern`), laid out as the kernel lays it: thread registers and file
//! mappings in notes, anonymous memory in the dump, file-backed pages left to the files on disk.

use std::path::PathBuf;

use nexo_dbg::{open, split_words, Kind, Session, State};

mod common;
use common::target;

fn run(s: &mut Session, cmd: &str) -> String {
    s.exec(&split_words(cmd)).unwrap_or_else(|e| panic!("{cmd}: {e}")).text_all()
}

fn note(out: &mut Vec<u8>, kind: u32, desc: &[u8]) {
    out.extend(5u32.to_le_bytes());
    out.extend((desc.len() as u32).to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(b"CORE\0\0\0\0");
    out.extend(desc);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// A core file of the program `s` is stopped in, written to `path`.
fn write_core(s: &mut Session, path: &std::path::Path) {
    let tid = s.t.threads()[0];
    let regs = s.t.regs(tid).unwrap();
    let maps = s.t.maps().unwrap();
    let mut notes = Vec::new();
    let mut st = vec![0u8; 336];
    st[0..4].copy_from_slice(&11u32.to_le_bytes());
    st[12..14].copy_from_slice(&11u16.to_le_bytes());
    st[32..36].copy_from_slice(&tid.to_le_bytes());
    for (i, w) in regs.to_user().iter().enumerate() {
        st[112 + i * 8..120 + i * 8].copy_from_slice(&w.to_le_bytes());
    }
    note(&mut notes, 1, &st);
    let mut si = vec![0u8; 128];
    si[0..4].copy_from_slice(&11u32.to_le_bytes());
    si[8..12].copy_from_slice(&1i32.to_le_bytes());
    note(&mut notes, 0x5349_4749, &si);
    let files: Vec<_> = maps.iter().filter(|m| m.path.starts_with('/')).collect();
    let mut nf = Vec::new();
    nf.extend((files.len() as u64).to_le_bytes());
    nf.extend(4096u64.to_le_bytes());
    for m in &files {
        nf.extend(m.start.to_le_bytes());
        nf.extend(m.end.to_le_bytes());
        nf.extend((m.offset / 4096).to_le_bytes());
    }
    for m in &files {
        nf.extend(m.path.as_bytes());
        nf.push(0);
    }
    note(&mut notes, 0x4649_4c45, &nf);
    // anonymous and stack memory goes in; file-backed pages that were not written to stay on disk
    let kept: Vec<_> = maps
        .iter()
        .filter(|m| {
            m.read
                && (m.path.is_empty() || m.path.starts_with('['))
                && !m.path.starts_with("[vv")
                && m.path != "[vsyscall]"
        })
        .collect();
    let nph = 1 + maps.iter().filter(|m| m.read && !m.path.starts_with("[vv") && m.path != "[vsyscall]").count();
    let mut data_at = 64 + 56 * nph + notes.len();
    let mut phdrs = Vec::new();
    let mut body = Vec::new();
    let ph = |kind: u32, flags: u32, off: u64, vaddr: u64, filesz: u64, memsz: u64| {
        let mut h = Vec::new();
        h.extend(kind.to_le_bytes());
        h.extend(flags.to_le_bytes());
        h.extend(off.to_le_bytes());
        h.extend(vaddr.to_le_bytes());
        h.extend(0u64.to_le_bytes());
        h.extend(filesz.to_le_bytes());
        h.extend(memsz.to_le_bytes());
        h.extend(1u64.to_le_bytes());
        h
    };
    phdrs.extend(ph(4, 0, 64 + 56 * nph as u64, 0, notes.len() as u64, 0));
    for m in maps.iter().filter(|m| m.read && !m.path.starts_with("[vv") && m.path != "[vsyscall]") {
        let flags = u32::from(m.read) * 4 + u32::from(m.write) * 2 + u32::from(m.exec);
        let len = m.end - m.start;
        if kept.iter().any(|k| k.start == m.start) {
            let mut b = vec![0u8; len as usize];
            let n = s.t.read(m.start, &mut b);
            assert_eq!(n, b.len(), "{m:?}");
            phdrs.extend(ph(1, flags, data_at as u64, m.start, len, len));
            data_at += b.len();
            body.extend(b);
        } else {
            phdrs.extend(ph(1, flags, 0, m.start, 0, len));
        }
    }
    let mut elf = Vec::new();
    elf.extend(b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0");
    elf.extend(4u16.to_le_bytes());
    elf.extend(62u16.to_le_bytes());
    elf.extend(1u32.to_le_bytes());
    elf.extend(0u64.to_le_bytes());
    elf.extend(64u64.to_le_bytes());
    elf.extend(0u64.to_le_bytes());
    elf.extend(0u32.to_le_bytes());
    elf.extend(64u16.to_le_bytes());
    elf.extend(56u16.to_le_bytes());
    elf.extend((nph as u16).to_le_bytes());
    elf.extend([0u8; 6]);
    elf.extend(phdrs);
    elf.extend(notes);
    elf.extend(body);
    std::fs::write(path, elf).unwrap();
}

fn crash_core(dir: &std::path::Path) -> (PathBuf, String, String) {
    let (mut s, _) = open(&Kind::Run { exe: target(), args: vec!["crash".into()] }).expect("start");
    let cont = run(&mut s, "cont --wait 10");
    assert!(cont.contains("SIGSEGV"), "{cont}");
    let regs = run(&mut s, "regs");
    let bt = run(&mut s, "bt");
    let path = dir.join("crash.core");
    write_core(&mut s, &path);
    let _ = run(&mut s, "kill");
    (path, regs, bt)
}

#[test]
fn a_core_file_opens_at_the_crash_with_the_registers_and_the_call_chain_it_had() {
    let dir = tempfile::tempdir().unwrap();
    let (path, regs, bt) = crash_core(dir.path());
    let (mut s, first) = open(&Kind::Core { file: path.clone(), exe: Some(target()) }).expect("open the core");
    let first = first.text_all();
    assert!(first.contains("core file") && first.contains("SIGSEGV"), "{first}");
    assert!(first.contains("pc 0x"), "{first}");
    assert_eq!(s.state, State::Stopped);
    // the same registers and the same chain as the live program had
    assert_eq!(run(&mut s, "regs"), regs);
    let again = run(&mut s, "bt");
    let names = |t: &str| t.lines().filter_map(|l| l.split_whitespace().nth(2).map(str::to_string)).collect::<Vec<_>>();
    let (core_names, live_names) = (names(&again), names(&bt));
    assert!(core_names.len() >= live_names.len() && core_names[..live_names.len()] == live_names[..], "{again}\n{bt}");
    // code is read from the file on disk, the stack from the dump
    let dis = run(&mut s, "dis");
    assert!(dis.contains("=>") || dis.contains("mov"), "{dis}");
    assert!(run(&mut s, "mem rsp 16").contains("0x"));
    assert!(run(&mut s, "modules").contains("dbg_target"));
    assert!(run(&mut s, "maps").contains("dbg_target"));
    // the program is gone: nothing runs or changes
    for c in ["cont", "step", "setreg rax 1", "write rsp 00"] {
        let e = s.exec(&split_words(c)).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("core file"), "{c}: {e}");
    }
}

#[test]
fn the_program_is_found_from_the_dump_and_a_file_that_is_not_a_core_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _, _) = crash_core(dir.path());
    let (_, first) = open(&Kind::Core { file: path, exe: None }).expect("the dump names its program");
    assert!(first.text_all().contains("SIGSEGV"));
    let e = open(&Kind::Core { file: target(), exe: None }).err().expect("an executable is no core").to_string();
    assert!(e.contains("not a core file"), "{e}");
    let junk = dir.path().join("junk");
    std::fs::write(&junk, b"hello").unwrap();
    let e = open(&Kind::Core { file: junk, exe: None }).err().expect("junk").to_string();
    assert!(e.contains("not an ELF"), "{e}");
}
