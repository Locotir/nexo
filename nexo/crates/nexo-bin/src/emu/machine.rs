//! The emulated machine: loading the image, stepping, calls into imports and libc.

use super::*;

impl<'a> Machine<'a> {
    pub(super) fn new(img: &'a Image, ms_abi: bool) -> Result<Machine<'a>> {
        let mut m = Machine {
            img,
            cpu: Cpu::new(),
            mem: Mem::default(),
            ms_abi,
            heap_next: HEAP_BASE + 0x10,
            blocks: BTreeMap::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            dropped_output: 0,
            calls: Vec::new(),
            calls_total: 0,
            regions: Vec::new(),
            imports: Vec::new(),
            overrides: HashMap::new(),
            last_call: 0,
            rsp0: 0,
        };
        m.load()?;
        Ok(m)
    }

    fn load(&mut self) -> Result<()> {
        let img = self.img;
        let mut mapped: u64 = 0;
        for s in img.sections.iter().filter(|s| s.alloc && s.size > 0) {
            let end = s.addr.saturating_add(s.size);
            if end > IMPORT_BASE {
                return Err(Error::Bad(format!(
                    "section {} at {:#x} is where the emulator keeps its stack and heap; emu cannot run this file",
                    s.name, s.addr
                )));
            }
            // sections are readable once loaded; ELF .bss and PE virtual sizes come as zeros. The first page
            // stays unmapped, as in a real process (a PIE file's headers sit there at its address 0), so a
            // null pointer faults
            let from = s.addr.max(mem::PAGE);
            if from >= end {
                continue;
            }
            // a header that claims a huge section must not make the emulator allocate it
            mapped = mapped.saturating_add(end - from);
            if mapped > MAX_IMAGE_BYTES {
                return Err(Error::Bad(format!(
                    "section {} at {:#x} makes the image {} MiB or more in memory; emu loads at most {} MiB (the file's section table may be damaged)",
                    s.name,
                    s.addr,
                    mapped >> 20,
                    MAX_IMAGE_BYTES >> 20
                )));
            }
            self.mem.map(from, end - from, s.write, s.exec);
            let bytes = img.section_bytes(s);
            let skip = (from - s.addr) as usize;
            let n = bytes.len().min(s.size as usize);
            if skip < n {
                let _ = self.mem.load(from, &bytes[skip..n]);
            }
        }
        if img.format == Format::Pe && img.image_base != 0 && !self.mem.is_mapped(img.image_base) {
            self.mem.map(img.image_base, mem::PAGE, false, false);
            let n = img.data.len().min(mem::PAGE as usize);
            let _ = self.mem.load(img.image_base, &img.data[..n]);
        }
        for &(slot, v) in &img.rel_ptrs {
            if self.mem.is_mapped(slot) {
                let _ = self.mem.load(slot, &v.to_le_bytes());
            }
        }
        // the three standard streams: a FILE value each, in cells the import slots can point at
        self.mem.map(FILE_BASE, mem::PAGE, true, false);
        for (k, name) in ["stdin", "stdout", "stderr"].iter().enumerate() {
            let cell = FILE_BASE + 0x800 + 8 * k as u64;
            let _ = self.mem.load(cell, &file_value(k).to_le_bytes());
            // a copy relocation puts the variable in the file's own .bss; fill it like the loader does
            for s in img.symbols.iter().filter(|s| !s.func && base_name(&s.name) == *name) {
                if self.mem.is_mapped(s.addr) {
                    let _ = self.mem.load(s.addr, &file_value(k).to_le_bytes());
                }
            }
        }
        for (i, im) in img.imports.iter().enumerate() {
            let name = base_name(&im.name).to_string();
            if let Some(slot) = im.slot {
                let v = match ["stdin", "stdout", "stderr"].iter().position(|s| *s == name) {
                    Some(k) => FILE_BASE + 0x800 + 8 * k as u64,
                    None => IMPORT_BASE + 16 * i as u64,
                };
                if self.mem.is_mapped(slot) {
                    let _ = self.mem.load(slot, &v.to_le_bytes());
                }
            }
            self.imports.push(name);
        }
        self.mem.map(TLS_BASE, 0x2000, true, false);
        self.cpu.fs_base = TLS_BASE + 0x1000;
        let _ = self.mem.load(TLS_BASE + 0x1000, &(TLS_BASE + 0x1000).to_le_bytes());
        let _ = self.mem.load(TLS_BASE + 0x1028, &CANARY.to_le_bytes());
        if self.ms_abi {
            self.cpu.gs_base = TLS_BASE + 0x1000;
            let _ = self.mem.load(TLS_BASE + 0x1030, &(TLS_BASE + 0x1000).to_le_bytes());
        }
        self.mem.map(STACK_TOP - STACK_SIZE, STACK_SIZE, true, false);
        Ok(())
    }

    /// Allocate `size` bytes in the heap (zeroed). `None` when the heap is full.
    pub(crate) fn alloc(&mut self, size: u64) -> Option<u64> {
        let a = self.heap_next;
        let end = a.checked_add(size.max(1))?;
        if end > HEAP_BASE + HEAP_MAX {
            return None;
        }
        self.mem.map(a, size.max(1), true, false);
        // 16 bytes of gap between blocks, so an overflow by one lands outside every block
        self.heap_next = (end + 16 + 15) & !15;
        let n = self.blocks.len() + 1;
        self.blocks.insert(a, Block { n, size, freed: false });
        Some(a)
    }

    /// Argument `i` (from 0) of the call being stubbed: rsp points at the return address.
    pub(crate) fn arg(&self, i: usize) -> std::result::Result<u64, Fault> {
        let regs: &[usize] = if self.ms_abi { &[1, 2, 8, 9] } else { &[7, 6, 2, 1, 8, 9] };
        if i < regs.len() {
            return Ok(self.cpu.gpr[regs[i]]);
        }
        let skip = if self.ms_abi { 8 + 32 } else { 8 };
        self.mem.read_u(self.cpu.gpr[RSP] + skip + 8 * (i - regs.len()) as u64, 8)
    }

    /// Program output, kept up to a bound.
    pub(crate) fn emit(&mut self, to_err: bool, bytes: &[u8]) {
        let used = self.stdout.len() + self.stderr.len();
        let room = MAX_OUTPUT.saturating_sub(used);
        let n = bytes.len().min(room);
        let buf = if to_err { &mut self.stderr } else { &mut self.stdout };
        buf.extend_from_slice(&bytes[..n]);
        self.dropped_output += bytes.len() - n;
    }

    /// What an address is, in a few words: an argument buffer, a heap block, the stack, a section...
    pub(crate) fn place(&self, a: u64) -> String {
        for (s, e, label) in &self.regions {
            if a >= *s && a < *e {
                return off(label, a - s);
            }
        }
        if let Some((&s, b)) = self.blocks.range(..=a).next_back() {
            if a < s + b.size.max(1) {
                let what = format!("heap block #{} ({} bytes{})", b.n, b.size, if b.freed { ", freed" } else { "" });
                return off(&what, a - s);
            }
        }
        if (STACK_TOP - STACK_SIZE..STACK_TOP).contains(&a) {
            return if a >= self.rsp0 {
                format!("stack rsp0+{:#x}", a - self.rsp0)
            } else {
                format!("stack rsp0-{:#x}", self.rsp0 - a)
            };
        }
        if (TLS_BASE..TLS_BASE + 0x2000).contains(&a) {
            let fs = TLS_BASE + 0x1000;
            return if a >= fs { format!("fs+{:#x}", a - fs) } else { format!("fs-{:#x}", fs - a) };
        }
        if a >= IMPORT_BASE && a < IMPORT_BASE + 16 * self.imports.len() as u64 {
            let i = ((a - IMPORT_BASE) / 16) as usize;
            return format!("the address nexo gave import {}", self.imports[i]);
        }
        if (RET_MAGIC..RET_MAGIC + 0x1000).contains(&a) {
            return "the return address nexo pushed".into();
        }
        if (FILE_BASE..FILE_BASE + mem::PAGE).contains(&a) {
            return "a FILE nexo made for stdin/stdout/stderr".into();
        }
        if (libc::CTYPE_BASE..libc::CTYPE_BASE + 3 * mem::PAGE).contains(&a) {
            return "the character class tables nexo made for <ctype.h>".into();
        }
        crate::disasm::describe(self.img, a, true).unwrap_or_else(|| "nothing mapped".into())
    }

    /// A pointer argument for the call log: the address and the C string there, when it reads as one.
    pub(crate) fn show_ptr(&self, a: u64) -> String {
        match self.mem.cstr(a, 48) {
            Ok((b, _)) if !b.is_empty() && b.iter().all(|&c| c == b'\t' || c == b'\n' || (0x20..0x7f).contains(&c)) => {
                format!("{a:#x} \"{}\"", escape(&b, 40))
            }
            _ => format!("{a:#x}"),
        }
    }

    pub(super) fn import_at(&self, rip: u64) -> Option<usize> {
        if rip < IMPORT_BASE || !(rip - IMPORT_BASE).is_multiple_of(16) {
            return None;
        }
        let i = ((rip - IMPORT_BASE) / 16) as usize;
        (i < self.imports.len()).then_some(i)
    }

    /// Run a stub for import `i`; `Ok` when the run goes on (rax set, back at the caller).
    pub(super) fn call_import(&mut self, i: usize) -> std::result::Result<(), Stop> {
        let name = self.imports[i].clone();
        let site = self.last_call;
        let (rax, text) = if let Some(&v) = self.overrides.get(&name) {
            (v, format!("{name}(...) = {v:#x} (--stub)"))
        } else {
            match libc::call(self, &name) {
                Some(Ok((v, args, shown))) => {
                    let shown = shown.unwrap_or_else(|| format!("{v:#x}"));
                    (v, format!("{name}({args}) = {shown}"))
                }
                Some(Err(stop)) => {
                    self.log_call(site, format!("{name}(...) stopped the run"));
                    return Err(stop);
                }
                None => return Err(Stop::UnknownImport { name, site }),
            }
        };
        self.log_call(site, text);
        let back = self.cpu.pop(&self.mem).map_err(|e| match e {
            Event::Fault(f) => Stop::Fault(f),
            _ => Stop::Abort("the return address of a library call cannot be read".into()),
        })?;
        self.cpu.gpr[RAX] = rax;
        self.cpu.rip = back;
        Ok(())
    }

    fn log_call(&mut self, site: u64, text: String) {
        self.calls_total += 1;
        if self.calls.len() < MAX_CALLS {
            self.calls.push(CallRec { site, text });
        }
    }

    pub(super) fn region(&mut self, data: &[u8], label: String) -> Result<u64> {
        let a = self
            .alloc(data.len() as u64)
            .ok_or_else(|| Error::Bad("the arguments do not fit in the emulator's heap".into()))?;
        let _ = self.mem.load(a, data);
        self.regions.push((a, a + data.len().max(1) as u64, label));
        // an argument buffer is not a heap block the program allocated
        self.blocks.remove(&a);
        Ok(a)
    }
}
