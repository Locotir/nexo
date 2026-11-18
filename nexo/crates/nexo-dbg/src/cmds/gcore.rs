//! `gcore`: an ELF core file of the stopped Linux process, so a hung or odd state can be kept and opened later
//! with `core FILE` (or gdb). The format is written by [`crate::coreout`]; this side gathers the process's facts.

use super::*;
use crate::coreout::{self, CoreIn, MapIn, ThreadIn};

/// The most a core file may take on disk; a bigger process is dumped in parts with `dump`.
const CORE_LIMIT: u64 = 4 << 30;

impl Session {
    pub(super) fn cmd_gcore(&mut self, a: &Args) -> Result<Output> {
        if a.pos.len() > 1 {
            return usage("gcore [FILE]");
        }
        self.need_stopped()?;
        let pid = self.t.pid();
        let proc = format!("/proc/{pid}");
        if self.t.os() != Os::Linux || !std::path::Path::new(&proc).is_dir() {
            return fail(
                "gcore writes the core of a live Linux process: this target is a core file, a remote stub or not Linux",
            );
        }
        let path = self.dump_target(a.pos.first().map(String::as_str), format!("core.{pid}"))?;
        // the stopped thread first: the core file is about it
        let mut tids = self.t.threads();
        tids.sort_unstable();
        tids.sort_by_key(|&t| t != self.cur);
        let mut threads = Vec::new();
        for tid in tids {
            let r = self.t.regs(tid)?;
            if r.x32 {
                return fail("gcore writes 64-bit cores: this thread runs 32-bit code");
            }
            // kind byte (0 XSAVE, 1 FXSAVE), then the FXSAVE area first in both
            let fpregs = self.t.vector_state(tid).and_then(|b| b.get(1..1 + coreout::FPREGS).map(<[u8]>::to_vec));
            threads.push(ThreadIn { tid, regs: r.to_user(), fpregs });
        }
        let mut maps = Vec::new();
        let mut total = 0u64;
        for m in self.t.maps()?.into_iter().filter(|m| coreout::kept(m.read, &m.path)) {
            let mut one = [0u8; 1];
            let readable = m.end > m.start && self.t.read(m.start, &mut one) == 1;
            let filesz = if readable { m.end - m.start } else { 0 };
            total = total.saturating_add(filesz);
            maps.push(MapIn {
                start: m.start,
                end: m.end,
                flags: coreout::elf_flags(m.read, m.write, m.exec),
                filesz,
                file_off: m.offset,
                path: m.path,
            });
        }
        if total > CORE_LIMIT {
            return fail(format!(
                "the process's readable memory is {} MiB, over the {} MiB a core may take: `dump` writes parts of it",
                total >> 20,
                CORE_LIMIT >> 20
            ));
        }
        let read = |f: &str| std::fs::read(format!("{proc}/{f}")).unwrap_or_default();
        let stat = String::from_utf8_lossy(&read("stat")).into_owned();
        let (name, ppid, pgrp, sid) = coreout::parse_stat(&stat).unwrap_or_default();
        let (uid, gid) = coreout::parse_ids(&String::from_utf8_lossy(&read("status")));
        let args = String::from_utf8_lossy(&read("cmdline")).trim_end_matches('\0').replace('\0', " ");
        let core = CoreIn { pid, ppid, pgrp, sid, uid, gid, name, args, auxv: read("auxv"), threads, maps };
        let (nthreads, nmaps) = (core.threads.len(), core.maps.len());
        let file = std::fs::File::create(&path).map_err(|e| crate::Error::Fail(format!("{}: {e}", path.display())))?;
        let mut out = std::io::BufWriter::new(file);
        let t = &mut *self.t;
        let mut rd = |addr: u64, buf: &mut [u8]| t.read(addr, buf);
        let size = match coreout::write_core(&mut out, &core, &mut rd) {
            Ok(n) => n,
            Err(e) => {
                drop(out);
                let _ = std::fs::remove_file(&path);
                return fail(format!("{}: {e}", path.display()));
            }
        };
        Ok(rows(
            "gcore",
            vec![
                format!("core written: {} ({} MiB)", path.display(), size >> 20),
                format!("{nthreads} threads, {nmaps} mappings; `core {}` opens it", path.display()),
            ],
            json!({"path": path.display().to_string(), "size": size, "threads": nthreads, "maps": nmaps}),
        ))
    }
}
