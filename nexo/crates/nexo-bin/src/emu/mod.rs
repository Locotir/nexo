//! `nexo bin emu`: run one function (or a stretch of code) of the file in an x86-64 emulator, without
//! running the program. Memory is the file's sections plus a stack, a small heap and the buffers the
//! arguments ask for; library calls go to stubs by name; an import with no stub, an instruction the
//! emulator does not know, or a bad memory access stops the run and says where and why. One answer gives
//! the return value, the reason it stopped, the memory written, the library calls and a short trace.

mod cpu;
mod fp;
mod libc;
mod mem;
mod sse;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fp;
mod vex;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Instant;

use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, Mnemonic};
use serde_json::{json, Value};

use crate::image::Format;
use crate::page::{shell_word, Output, Row};
use crate::{parse_addr, parse_number, Error, Image, Result};
use cpu::{Cpu, Event, GPR_NAMES, RAX, RSP};
use mem::{Access, Fault, Mem};

/// Where nexo puts the things the file does not have. All far from where ELF and PE files load.
const IMPORT_BASE: u64 = 0x7a00_0000_0000;
const FILE_BASE: u64 = 0x7b00_0000_0000;
/// The most bytes one library stub copies, compares or allocates.
const MAX_LIB_BYTES: u64 = 64 << 20;
const RET_MAGIC: u64 = 0x7c00_dead_0000;
const TLS_BASE: u64 = 0x7d00_0000_0000;
const HEAP_BASE: u64 = 0x7e00_0000_0000;
const HEAP_MAX: u64 = 64 << 20;
const STACK_TOP: u64 = 0x7fff_f000_0000;
const STACK_SIZE: u64 = 1 << 20;
/// The value at fs:0x28 (the stack protector's canary).
const CANARY: u64 = 0x2f8a_17c3_9b04_5e00;

/// Default and largest step budgets and time limits.
pub const DEFAULT_STEPS: u64 = 2_000_000;
pub const MAX_STEPS: u64 = 200_000_000;
pub const DEFAULT_SECS: u64 = 5;
pub const MAX_SECS: u64 = 60;
pub const DEFAULT_TRACE: usize = 12;
pub const MAX_TRACE: usize = 200;
/// Bytes of program output kept, library calls logged, bytes shown per written range by default.
const MAX_OUTPUT: usize = 4096;
const MAX_CALLS: usize = 40;
pub const DEFAULT_DUMP: usize = 64;
pub const MAX_DUMP: usize = 4096;
/// The largest buffer an argument may ask for.
const MAX_ARG_BUF: usize = 1 << 20;

/// The defaults and bounds of the options.
pub mod limits {
    pub use super::{
        DEFAULT_DUMP, DEFAULT_SECS, DEFAULT_STEPS, DEFAULT_TRACE, MAX_DUMP, MAX_SECS, MAX_STEPS, MAX_TRACE,
    };
}

/// What to run and how. Values (`args`, `--reg`, `--mem`) are written as in [`parse_value`].
#[derive(Debug, Clone)]
pub struct EmuSpec {
    /// A function name, or an address to start at.
    pub target: String,
    pub args: Vec<String>,
    /// Stop when the program counter reaches this address.
    pub to: Option<u64>,
    /// `NAME=VALUE` presets for general registers.
    pub regs: Vec<String>,
    /// `ADDR=VALUE` presets for memory.
    pub mem: Vec<String>,
    /// `NAME=NUMBER`: the import returns NUMBER and does nothing else (overrides the built-in stub).
    pub stubs: Vec<String>,
    /// `sysv` or `ms`; by default ms for PE files, sysv otherwise.
    pub abi: Option<String>,
    pub steps: u64,
    pub secs: u64,
    pub trace: usize,
    /// Bytes shown per written range.
    pub dump: usize,
    /// Show the stack writes too.
    pub stack: bool,
    /// `ADDR:LEN` regions to show after the run.
    pub show: Vec<String>,
}

impl EmuSpec {
    pub fn new(target: &str) -> EmuSpec {
        EmuSpec {
            target: target.to_string(),
            args: Vec::new(),
            to: None,
            regs: Vec::new(),
            mem: Vec::new(),
            stubs: Vec::new(),
            abi: None,
            steps: DEFAULT_STEPS,
            secs: DEFAULT_SECS,
            trace: DEFAULT_TRACE,
            dump: DEFAULT_DUMP,
            stack: false,
            show: Vec::new(),
        }
    }
}

/// Why the run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    Returned,
    Reached(u64),
    Steps,
    Time,
    UnknownImport {
        name: String,
        site: u64,
    },
    Unsupported(String),
    Undecodable,
    Fault(Fault),
    Divide,
    Trap(&'static str),
    Syscall(u64),
    Exit(i64),
    /// A stub ended the run: abort, a failed assert, the stack protector, a bad free...
    Abort(String),
    /// A stub was handed memory it cannot read or write.
    LibFault {
        call: String,
        fault: Fault,
    },
}

impl Stop {
    fn kind(&self) -> &'static str {
        match self {
            Stop::Returned => "returned",
            Stop::Reached(_) => "reached",
            Stop::Steps => "step_cap",
            Stop::Time => "time_cap",
            Stop::UnknownImport { .. } => "unknown_import",
            Stop::Unsupported(_) => "unsupported",
            Stop::Undecodable => "undecodable",
            Stop::Fault(_) => "fault",
            Stop::Divide => "divide_error",
            Stop::Trap(_) => "trap",
            Stop::Syscall(_) => "syscall",
            Stop::Exit(_) => "exit",
            Stop::Abort(_) => "abort",
            Stop::LibFault { .. } => "library_fault",
        }
    }
}

/// A block the heap stubs handed out.
#[derive(Debug, Clone)]
pub(crate) struct Block {
    pub n: usize,
    pub size: u64,
    pub freed: bool,
}

/// One library call the run made.
#[derive(Debug, Clone)]
struct CallRec {
    site: u64,
    text: String,
}

pub(crate) struct Machine<'a> {
    img: &'a Image,
    pub cpu: Cpu,
    pub mem: Mem,
    pub ms_abi: bool,
    heap_next: u64,
    pub blocks: BTreeMap<u64, Block>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub dropped_output: usize,
    calls: Vec<CallRec>,
    calls_total: usize,
    /// Buffers made for arguments and presets: (start, end, label).
    regions: Vec<(u64, u64, String)>,
    imports: Vec<String>,
    overrides: HashMap<String, u64>,
    /// The last `call` instruction run (the site a library call is reported at).
    last_call: u64,
    rsp0: u64,
}

impl<'a> Machine<'a> {
    fn new(img: &'a Image, ms_abi: bool) -> Result<Machine<'a>> {
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

    fn import_at(&self, rip: u64) -> Option<usize> {
        if rip < IMPORT_BASE || !(rip - IMPORT_BASE).is_multiple_of(16) {
            return None;
        }
        let i = ((rip - IMPORT_BASE) / 16) as usize;
        (i < self.imports.len()).then_some(i)
    }

    /// Run a stub for import `i`; `Ok` when the run goes on (rax set, back at the caller).
    fn call_import(&mut self, i: usize) -> std::result::Result<(), Stop> {
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

    fn region(&mut self, data: &[u8], label: String) -> Result<u64> {
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

/// Answer lines keyed by position; written ranges carry a JSON item.
struct Rows(Vec<Row>);

impl Rows {
    fn push(&mut self, t: impl Into<String>) {
        let key = self.0.len() as u64;
        self.0.push(Row { key, text: t.into(), json: None });
    }
    fn item(&mut self, t: impl Into<String>, j: Value) {
        let key = self.0.len() as u64;
        self.0.push(Row { key, text: t.into(), json: Some(j) });
    }
}

fn off(label: &str, d: u64) -> String {
    if d == 0 {
        label.to_string()
    } else {
        format!("{label}+{d:#x}")
    }
}

/// The FILE pointer value nexo gives stdin (0), stdout (1) and stderr (2).
pub(crate) fn file_value(k: usize) -> u64 {
    FILE_BASE + 0x100 * k as u64
}

/// `stdout@GLIBC_2.2.5` -> `stdout`.
fn base_name(s: &str) -> &str {
    s.split('@').next().unwrap_or(s)
}

/// Bytes as text for a quoted string: printable ASCII as is, the rest escaped, cut at `max` characters.
pub(crate) fn escape(b: &[u8], max: usize) -> String {
    let mut out = String::new();
    for (n, &c) in b.iter().enumerate() {
        if n == max {
            out.push_str(&format!("...(+{} bytes)", b.len() - max));
            break;
        }
        match c {
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out
}

/// A value for an argument, a register or memory:
/// - a number: `42`, `-1`, `0x401000`
/// - `str:TEXT`: the text plus a NUL in a new buffer; the value is its address
/// - `hex:DE AD BE EF`: those bytes in a new buffer
/// - `buf:N`: N zero bytes in a new buffer (somewhere for the function to write)
enum Value_ {
    Num(u64),
    Bytes(Vec<u8>, &'static str),
}

fn parse_value(s: &str) -> Result<Value_> {
    if let Some(t) = s.strip_prefix("str:") {
        let mut b = t.as_bytes().to_vec();
        b.push(0);
        return Ok(Value_::Bytes(b, "str"));
    }
    if let Some(t) = s.strip_prefix("hex:") {
        let clean: String = t.chars().filter(|c| !c.is_whitespace()).collect();
        if !clean.len().is_multiple_of(2) || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Bad(format!("{s:?}: hex: takes pairs of hex digits, like hex:de ad be ef")));
        }
        let b = (0..clean.len()).step_by(2).map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap_or(0)).collect();
        return Ok(Value_::Bytes(b, "hex"));
    }
    if let Some(t) = s.strip_prefix("buf:") {
        let n = parse_addr(t)? as usize;
        if n == 0 || n > MAX_ARG_BUF {
            return Err(Error::Bad(format!("{s:?}: buf: takes a size from 1 to {MAX_ARG_BUF}")));
        }
        return Ok(Value_::Bytes(vec![0; n], "buf"));
    }
    match parse_number(s) {
        Ok(v) => Ok(Value_::Num(v as u64)),
        Err(_) => Err(Error::Bad(format!(
            "{s:?} is not a value: write a number (42, -1, 0x401000), str:TEXT, hex:DE AD or buf:SIZE"
        ))),
    }
}

/// The value to put in a register for `s`, making the buffer it asks for.
fn value_in(m: &mut Machine, s: &str, label: String) -> Result<(u64, Option<String>)> {
    match parse_value(s)? {
        Value_::Num(v) => Ok((v, None)),
        Value_::Bytes(b, kind) => {
            let n = b.len();
            let label = format!("{label} {kind}");
            let a = m.region(&b, label.clone())?;
            let what = match kind {
                "str" => format!("{label} \"{}\"", escape(&b[..n - 1], 40)),
                _ => format!("{label}, {n} bytes"),
            };
            Ok((a, Some(what)))
        }
    }
}

fn reg_named(name: &str) -> Option<usize> {
    GPR_NAMES.iter().position(|r| r.eq_ignore_ascii_case(name.trim()))
}

/// Run the function or stretch of code `spec` names and describe what happened.
pub fn emulate(img: &Image, spec: &EmuSpec) -> Result<Output> {
    let t0 = Instant::now();
    let ms_abi = match spec.abi.as_deref() {
        None => img.format == Format::Pe,
        Some("sysv") => false,
        Some("ms") => true,
        Some(o) => return Err(Error::Usage(format!("--abi {o:?}: write sysv or ms"))),
    };
    let start = start_of(img, &spec.target)?;
    let mut m = Machine::new(img, ms_abi)?;
    for s in &spec.stubs {
        let (n, v) = s
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--stub {s:?}: write NAME=NUMBER, like --stub rand=4")))?;
        let v = parse_number(v).map_err(|e| Error::Usage(format!("--stub {s:?}: {e}")))? as u64;
        m.overrides.insert(n.trim().to_string(), v);
    }
    // memory presets first, so an argument may point into them
    for s in &spec.mem {
        let (a, v) = s
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--mem {s:?}: write ADDR=VALUE, like --mem 0x4010=hex:01 02")))?;
        let a = parse_addr(a)?;
        let bytes = match v.split_once(':') {
            Some(("u8", n)) => (parse_number(n)? as u64).to_le_bytes()[..1].to_vec(),
            Some(("u16", n)) => (parse_number(n)? as u64).to_le_bytes()[..2].to_vec(),
            Some(("u32", n)) => (parse_number(n)? as u64).to_le_bytes()[..4].to_vec(),
            Some(("u64", n)) => (parse_number(n)? as u64).to_le_bytes().to_vec(),
            _ => match parse_value(v)? {
                Value_::Bytes(b, "buf") => b,
                Value_::Bytes(b, _) => b,
                Value_::Num(_) => {
                    return Err(Error::Usage(format!(
                        "--mem {s:?}: say the width of a number (u8:, u16:, u32:, u64:) or give str:/hex:"
                    )))
                }
            },
        };
        if (0..bytes.len() as u64).any(|i| !m.mem.is_mapped(a + i)) {
            return Err(Error::Bad(format!(
                "--mem {s:?}: {a:#x} is not in a section of the file; to hand the function a buffer, pass buf:SIZE or hex:... as an argument"
            )));
        }
        let _ = m.mem.load(a, &bytes);
    }
    // arguments
    let regs: &[usize] = if ms_abi { &[1, 2, 8, 9] } else { &[7, 6, 2, 1, 8, 9] };
    let mut stack_args = Vec::new();
    let mut shown_args = Vec::new();
    for (i, s) in spec.args.iter().enumerate() {
        let (v, what) = value_in(&mut m, s, format!("arg{}", i + 1))?;
        let at = match regs.get(i) {
            Some(&r) => {
                m.cpu.gpr[r] = v;
                GPR_NAMES[r].to_string()
            }
            None => {
                stack_args.push(v);
                format!("stack arg {}", stack_args.len())
            }
        };
        shown_args.push(match what {
            Some(w) => format!("{at}={v:#x} ({w})"),
            None => format!("{at}={}", num_at(img, v)),
        });
    }
    for s in &spec.regs {
        let (n, v) = s
            .split_once('=')
            .ok_or_else(|| Error::Usage(format!("--reg {s:?}: write NAME=VALUE, like --reg rbx=5")))?;
        let r = reg_named(n)
            .ok_or_else(|| Error::Usage(format!("--reg {s:?}: {n} is not a 64-bit general register (rax..r15)")))?;
        if r == RSP {
            return Err(Error::Usage("--reg rsp: the stack pointer is set by emu".into()));
        }
        let (v, what) = value_in(&mut m, v, GPR_NAMES[r].to_string())?;
        m.cpu.gpr[r] = v;
        shown_args.push(match what {
            Some(w) => format!("{}={v:#x} ({w})", GPR_NAMES[r]),
            None => format!("{}={}", GPR_NAMES[r], num_at(img, v)),
        });
    }
    // the stack: arguments past the registers, the home area on Windows, then the return address
    let shadow = if ms_abi { 4 } else { 0 };
    let mut sp = STACK_TOP - 0x4000;
    sp -= 8 * (stack_args.len() + shadow) as u64;
    sp &= !15;
    for (k, v) in stack_args.iter().enumerate() {
        let _ = m.mem.load(sp + 8 * (shadow + k) as u64, &v.to_le_bytes());
    }
    sp -= 8;
    let _ = m.mem.load(sp, &RET_MAGIC.to_le_bytes());
    m.cpu.gpr[RSP] = sp;
    m.rsp0 = sp;
    m.cpu.rip = start;
    let initial = m.cpu.gpr;

    let budget = spec.steps.clamp(1, MAX_STEPS);
    let limit = std::time::Duration::from_secs(spec.secs.clamp(1, MAX_SECS));
    let trace_n = spec.trace.min(MAX_TRACE);
    let mut trace: VecDeque<(Instruction, Vec<(usize, u64)>)> = VecDeque::with_capacity(trace_n + 1);
    let mut cache: HashMap<u64, Instruction> = HashMap::new();
    let mut steps: u64 = 0;
    let mut next_clock: u64 = 0;
    let stop = loop {
        let rip = m.cpu.rip;
        if rip == RET_MAGIC {
            break Stop::Returned;
        }
        if Some(rip) == spec.to && steps > 0 {
            break Stop::Reached(rip);
        }
        if let Some(i) = m.import_at(rip) {
            match m.call_import(i) {
                Ok(()) => continue,
                Err(s) => break s,
            }
        }
        if steps >= budget {
            break Stop::Steps;
        }
        if steps >= next_clock {
            next_clock = steps + 4096;
            if t0.elapsed() > limit {
                break Stop::Time;
            }
        }
        if m.mem.code_written {
            cache.clear();
            m.mem.code_written = false;
        }
        let ins = match cache.get(&rip) {
            Some(i) => *i,
            None => {
                let mut buf = [0u8; 15];
                let n = match m.mem.fetch(rip, &mut buf) {
                    Ok(n) => n,
                    Err(f) => break Stop::Fault(f),
                };
                let ins = Decoder::with_ip(64, &buf[..n], rip, DecoderOptions::NONE).decode();
                if ins.is_invalid() {
                    break Stop::Undecodable;
                }
                cache.insert(rip, ins);
                ins
            }
        };
        let before = m.cpu.gpr;
        m.cpu.rip = ins.next_ip();
        m.cpu.budget = budget - steps - 1;
        m.cpu.extra_steps = 0;
        if ins.mnemonic() == Mnemonic::Call {
            m.last_call = rip;
        }
        let done = match m.cpu.exec(&mut m.mem, &ins) {
            Ok(true) => Ok(true),
            Ok(false) => match sse::exec(&mut m.cpu, &mut m.mem, &ins) {
                Ok(false) => fp::exec(&mut m.cpu, &mut m.mem, &ins),
                other => other,
            },
            Err(e) => Err(e),
        };
        steps += 1 + m.cpu.extra_steps;
        let ev = match done {
            Ok(true) => None,
            Ok(false) => Some(Event::Unsupported(String::new())),
            Err(e) => Some(e),
        };
        if trace_n > 0 {
            let changed = (0..16).filter(|&r| m.cpu.gpr[r] != before[r]).map(|r| (r, m.cpu.gpr[r])).collect();
            if trace.len() == trace_n {
                trace.pop_front();
            }
            trace.push_back((ins, changed));
        }
        if let Some(e) = ev {
            m.cpu.rip = rip;
            break match e {
                Event::Fault(f) => Stop::Fault(f),
                Event::Unsupported(d) => Stop::Unsupported(d),
                Event::DivideError => Stop::Divide,
                Event::Trap(t) => Stop::Trap(t),
                Event::Syscall(n) => Stop::Syscall(n),
                Event::Budget => Stop::Steps,
            };
        }
    };
    Ok(report(img, spec, &m, &stop, start, steps, t0, &initial, &shown_args, &trace))
}

/// The address to start at: a function's start for a name, the address itself for a number.
fn start_of(img: &Image, target: &str) -> Result<u64> {
    if crate::ops::numeric(target) {
        let a = parse_addr(target)?;
        return match img.section_at(a) {
            Some(s) if s.exec => Ok(a),
            Some(s) => Err(Error::Bad(format!("{a:#x} is in {}, which is data, not code", s.name))),
            None => Err(Error::Bad(format!("{a:#x} is not inside any section (list them: nexo bin info)"))),
        };
    }
    Ok(crate::ops::resolve(img, target)?.start)
}

fn num(v: u64) -> String {
    let s = v as i64;
    if v < 10 {
        format!("{v}")
    } else if (-4096..0).contains(&s) {
        format!("{v:#x} ({s})")
    } else {
        format!("{v:#x} ({v})")
    }
}

/// Byte ranges, start to end (exclusive).
type Ranges = Vec<(u64, u64)>;

/// A number, and what it points at when it is an address in the file.
fn num_at(img: &Image, v: u64) -> String {
    match img.section_at(v).and_then(|_| crate::disasm::describe(img, v, true)) {
        Some(d) if v >= mem::PAGE => format!("{v:#x} ({d})"),
        _ => num(v),
    }
}

/// Printable runs of at least 4 bytes in `b`, as (offset, text).
fn text_runs(b: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let printable = |c: u8| (0x20..0x7f).contains(&c) || c == b'\n' || c == b'\t';
        if printable(b[i]) {
            let s = i;
            while i < b.len() && printable(b[i]) {
                i += 1;
            }
            if i - s >= 4 {
                out.push((s, escape(&b[s..i], 80)));
            }
        } else {
            i += 1;
        }
    }
    out
}

fn hexdump(m: &Machine, start: u64, len: usize, rows: &mut Rows) {
    let mut buf = vec![0u8; len];
    let mut ok = 0;
    for (k, b) in buf.iter_mut().enumerate() {
        match m.mem.read_u(start + k as u64, 1) {
            Ok(v) => {
                *b = v as u8;
                ok = k + 1;
            }
            Err(_) => break,
        }
    }
    for (k, chunk) in buf[..ok].chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let asc: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        rows.push(format!("    {:#x}  {:<47}  |{asc}|", start + 16 * k as u64, hex.join(" ")));
    }
    if ok < len {
        rows.push(format!("    {:#x}  (not mapped)", start + ok as u64));
    }
}

#[allow(clippy::too_many_arguments)]
fn report(
    img: &Image,
    spec: &EmuSpec,
    m: &Machine,
    stop: &Stop,
    start: u64,
    steps: u64,
    t0: Instant,
    initial: &[u64; 16],
    args: &[String],
    trace: &VecDeque<(Instruction, Vec<(usize, u64)>)>,
) -> Output {
    let mut fmt = crate::disasm::formatter();
    let ins_text = |fmt: &mut iced_x86::IntelFormatter, ins: &Instruction| {
        let mut t = String::new();
        fmt.format(ins, &mut t);
        t
    };
    let at = |a: u64| match img.label(a) {
        Some(l) => format!("{a:#x} {l}"),
        None => format!("{a:#x}"),
    };
    let rip = m.cpu.rip;
    let cur = |fmt: &mut iced_x86::IntelFormatter| {
        let mut buf = [0u8; 15];
        match m.mem.fetch(rip, &mut buf) {
            Ok(n) => {
                let i = Decoder::with_ip(64, &buf[..n], rip, DecoderOptions::NONE).decode();
                if i.is_invalid() {
                    format!("bytes {}", crate::ops::hex(&buf[..n.min(8)]))
                } else {
                    format!("`{}`", ins_text(fmt, &i))
                }
            }
            Err(_) => "(no code there)".into(),
        }
    };
    let rerun = |extra: &str| {
        let mut words = vec![shell_word(&spec.target)];
        words.extend(spec.args.iter().map(|a| shell_word(a)));
        format!("{} {extra}", crate::ops::command(img, "emu", &words.join(" ")))
    };
    let stop_text = match stop {
        Stop::Returned => "returned to the caller".to_string(),
        Stop::Reached(a) => format!("reached --to {}", at(*a)),
        Stop::Steps => format!(
            "step cap ({steps} steps) at {}: a loop that does not end, or a long one (raise it: {})",
            at(rip),
            rerun(&format!("--steps {}", (spec.steps.saturating_mul(10)).min(MAX_STEPS)))
        ),
        Stop::Time => format!("time cap ({} s) at {} after {steps} steps", spec.secs, at(rip)),
        Stop::UnknownImport { name, site } => format!(
            "call to import `{name}` (no stub) from {}; give it a return value: {}",
            at(*site),
            rerun(&format!("--stub {name}=0"))
        ),
        Stop::Unsupported(d) => format!(
            "unsupported instruction {} at {}{} (stopped before it; nothing was skipped)",
            cur(&mut fmt),
            at(rip),
            if d.is_empty() { String::new() } else { format!(": {d}") }
        ),
        Stop::Undecodable => format!("bytes that do not decode as an instruction at {}: {}", at(rip), cur(&mut fmt)),
        Stop::Fault(f) => fault_text(m, f, &at(rip), &cur(&mut fmt)),
        Stop::Divide => format!("divide error (by zero or overflow) at {} {}", at(rip), cur(&mut fmt)),
        Stop::Trap(t) => format!("{t} at {}: the program traps here", at(rip)),
        Stop::Syscall(n) => format!("system call {n} at {}: emu does not make system calls", at(rip)),
        Stop::Exit(c) => format!("the program called exit({c})"),
        Stop::Abort(why) => why.clone(),
        Stop::LibFault { call, fault } => format!(
            "{call}: {} {:#x} ({}), {}",
            fault.access.word(),
            fault.addr,
            m.place(fault.addr),
            if fault.mapped { "not allowed there" } else { "nothing mapped there" }
        ),
    };
    let mut rows = Rows(Vec::new());
    let abi = if m.ms_abi { "ms" } else { "sysv" };
    rows.push(format!(
        "emu {} from {}  abi {abi}  {steps} steps  {:.1} ms",
        img.path.display(),
        at(start),
        t0.elapsed().as_secs_f64() * 1000.0
    ));
    if !args.is_empty() {
        rows.push(format!("in        {}", args.join("  ")));
    }
    rows.push(format!("stop      {stop_text}"));
    let rax = m.cpu.gpr[RAX];
    if *stop == Stop::Returned {
        let x0 = m.cpu.xmm[0];
        let extra =
            if x0 != 0 { format!("  xmm0={x0:#x} (as double {})", f64::from_bits(x0 as u64)) } else { String::new() };
        rows.push(format!(
            "return    rax={} eax={} al={}{extra}",
            num(rax),
            num(rax & 0xffff_ffff).replace("(-", "(as int -"),
            rax & 0xff
        ));
    } else {
        rows.push(format!("at        {} {}", at(rip), cur(&mut fmt)));
    }
    let changed: Vec<String> = (0..16)
        .filter(|&r| r != RSP && (m.cpu.gpr[r] != initial[r] || *stop != Stop::Returned))
        .filter(|&r| m.cpu.gpr[r] != 0 || initial[r] != 0)
        .map(|r| format!("{}={:#x}", GPR_NAMES[r], m.cpu.gpr[r]))
        .collect();
    if !changed.is_empty() {
        let head = if *stop == Stop::Returned { "regs out " } else { "regs     " };
        rows.push(format!("{head} {}", changed.join(" ")));
    }
    // library calls
    if m.calls_total > 0 {
        rows.push(format!("calls     {} library call(s)", m.calls_total));
        for c in &m.calls {
            rows.push(format!("  {:#x}  {}", c.site, c.text));
        }
        if m.calls_total > m.calls.len() {
            rows.push(format!("  ... {} more calls not listed", m.calls_total - m.calls.len()));
        }
    }
    for (name, buf) in [("stdout", &m.stdout), ("stderr", &m.stderr)] {
        if !buf.is_empty() {
            rows.push(format!("{name:<9} \"{}\" ({} bytes)", escape(buf, 2000), buf.len()));
        }
    }
    if m.dropped_output > 0 {
        rows.push(format!("output    {} more bytes of output not kept", m.dropped_output));
    }
    // memory written
    let stack = STACK_TOP - STACK_SIZE..STACK_TOP;
    let (on_stack, off_stack): (Ranges, Ranges) = m.mem.written().partition(|(s, _)| stack.contains(s));
    let total: u64 = off_stack.iter().map(|(s, e)| e - s).sum();
    if off_stack.is_empty() {
        rows.push("memory    nothing written outside the stack");
    } else {
        rows.push(format!("memory    {} range(s), {total} bytes written outside the stack", off_stack.len()));
    }
    let dump = spec.dump.clamp(16, MAX_DUMP);
    let show_range = |rows: &mut Rows, s: u64, e: u64| {
        let len = (e - s) as usize;
        let mut b = vec![0u8; len.min(MAX_ARG_BUF)];
        let _ = m.mem.read(s, &mut b);
        let runs = text_runs(&b);
        let item = json!({"addr": s, "len": len, "place": m.place(s),
            "text": runs.iter().map(|(o, t)| json!({"addr": s + *o as u64, "text": t})).collect::<Vec<_>>()});
        rows.item(format!("  {s:#x} +{len}  {}", m.place(s)), item);
        for (o, t) in runs.iter().take(4) {
            rows.push(format!("    text at {:#x}: \"{t}\"", s + *o as u64));
        }
        if runs.len() > 4 {
            rows.push(format!("    ... {} more text runs", runs.len() - 4));
        }
        hexdump(m, s, len.min(dump), rows);
        if len > dump {
            rows.push(format!("    ... {} more bytes (see them: --show {:#x}:{len})", len - dump, s + dump as u64));
        }
    };
    for &(s, e) in &off_stack {
        show_range(&mut rows, s, e);
    }
    let stack_bytes: u64 = on_stack.iter().map(|(s, e)| e - s).sum();
    if stack_bytes > 0 {
        if spec.stack {
            rows.push(format!("stack     {} range(s), {stack_bytes} bytes written", on_stack.len()));
            for &(s, e) in &on_stack {
                show_range(&mut rows, s, e);
            }
        } else {
            let mut texts = Vec::new();
            for &(s, e) in &on_stack {
                let mut b = vec![0u8; (e - s) as usize];
                let _ = m.mem.read(s, &mut b);
                texts.extend(
                    text_runs(&b)
                        .into_iter()
                        .map(|(o, t)| format!("{:#x} ({}) \"{t}\"", s + o as u64, m.place(s + o as u64))),
                );
            }
            rows.push(format!(
                "stack     {stack_bytes} bytes written in {} range(s) (show them: --stack){}",
                on_stack.len(),
                if texts.is_empty() { String::new() } else { "; text there:".into() }
            ));
            for t in texts.iter().take(8) {
                rows.push(format!("    {t}"));
            }
            if texts.len() > 8 {
                rows.push(format!("    ... {} more (--stack)", texts.len() - 8));
            }
        }
    }
    for s in &spec.show {
        let parsed = s.split_once(':').and_then(|(a, l)| Some((parse_addr(a).ok()?, parse_addr(l).ok()?)));
        match parsed {
            Some((a, l)) => {
                let l = (l as usize).clamp(1, MAX_DUMP);
                rows.push(format!("show      {a:#x} +{l}  {}", m.place(a)));
                hexdump(m, a, l, &mut rows);
            }
            None => rows.push(format!("show      {s:?}: write ADDR:LEN, like --show 0x7e0000000010:64")),
        }
    }
    // trace tail
    if !trace.is_empty() {
        rows.push(format!("trace     last {} of {steps} steps", trace.len()));
        for (ins, ch) in trace {
            let mut line = format!("  {}  {}", at(ins.ip()), ins_text(&mut fmt, ins));
            if let Some(c) = crate::disasm::comment(img, ins) {
                line.push_str(&format!("  ; {c}"));
            }
            if !ch.is_empty() {
                let regs: Vec<String> =
                    ch.iter().filter(|(r, _)| *r != RSP).map(|(r, v)| format!("{}={v:#x}", GPR_NAMES[*r])).collect();
                if !regs.is_empty() {
                    line.push_str(&format!("  -> {}", regs.join(" ")));
                }
            }
            rows.push(line);
        }
    }
    let meta = json!({
        "file": img.path.display().to_string(),
        "start": start,
        "abi": abi,
        "steps": steps,
        "stop": {"kind": stop.kind(), "text": stop_text, "rip": rip, "place": img.label(rip)},
        "return": (*stop == Stop::Returned).then_some(rax),
        "regs": (0..16).map(|r| (GPR_NAMES[r].to_string(), json!(m.cpu.gpr[r]))).collect::<serde_json::Map<_, _>>(),
        "stdout": String::from_utf8_lossy(&m.stdout),
        "stderr": String::from_utf8_lossy(&m.stderr),
        "calls": m.calls.iter().map(|c| json!({"site": c.site, "call": c.text})).collect::<Vec<_>>(),
    });
    let rows = rows.0;
    let mut words = vec![shell_word(&spec.target)];
    words.extend(spec.args.iter().map(|a| shell_word(a)));
    Output { op: "emu", meta, rows, tail: 0, command: crate::ops::command(img, "emu", &words.join(" ")) }
}

fn fault_text(m: &Machine, f: &Fault, at: &str, cur: &str) -> String {
    let why = if f.access == Access::Exec {
        if f.mapped {
            "jumped into memory that is not code".to_string()
        } else {
            "jumped to an address where nothing is mapped".to_string()
        }
    } else if f.mapped {
        format!("{} not allowed", f.access.word())
    } else {
        format!("{} of unmapped memory", f.access.word())
    };
    if f.access == Access::Exec {
        format!("{why}: {:#x} ({})", f.addr, m.place(f.addr))
    } else {
        format!("{why} {:#x} ({}) at {at} {cur}", f.addr, m.place(f.addr))
    }
}
