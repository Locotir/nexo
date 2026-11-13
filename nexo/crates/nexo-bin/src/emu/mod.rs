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

mod machine;
mod report;
use self::report::*;

/// Where nexo puts the things the file does not have. All far from where ELF and PE files load.
/// The most memory the sections of one file may take once loaded (zero pages count).
const MAX_IMAGE_BYTES: u64 = 256 << 20;
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
    img.require_x86("emulate")?;
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
