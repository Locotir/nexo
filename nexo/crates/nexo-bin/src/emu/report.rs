//! The emulation answer: number and text formatting, memory dumps and the final report rows.

use super::*;

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
pub(super) fn num_at(img: &Image, v: u64) -> String {
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
pub(super) fn report(
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
