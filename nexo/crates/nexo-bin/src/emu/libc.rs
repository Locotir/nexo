//! Library calls by name: the C string, memory, heap and output functions small programs use. A name with
//! no stub here returns `None` and the run stops on it, naming the import.

use super::mem::{Fault, PAGE};
use super::{file_value, Block, Machine, Stop, FILE_BASE, MAX_LIB_BYTES};

type Answer = Result<(u64, String, Option<String>), Stop>;

/// Where the character class tables for `__ctype_b_loc` and friends live (made on first use).
pub(super) const CTYPE_BASE: u64 = FILE_BASE + 0x10000;

/// Run the stub for `name`: the value for rax, the arguments as text and, when it reads better than hex,
/// the value as text. `None` when there is no stub.
pub(super) fn call(m: &mut Machine, name: &str) -> Option<Answer> {
    let name = name.strip_prefix("__imp_").unwrap_or(name);
    let r = match name {
        "strlen" | "strnlen" => strlen(m, name),
        "strcmp" | "strncmp" | "strcasecmp" | "strncasecmp" | "_stricmp" | "_strnicmp" | "strcoll" => strcmp(m, name),
        "memcmp" | "bcmp" => memcmp(m, name),
        "memcpy" | "memmove" | "mempcpy" | "__memcpy_chk" | "__memmove_chk" | "__mempcpy_chk" => memcpy(m, name),
        "memset" | "__memset_chk" => memset(m, name),
        "bzero" | "explicit_bzero" => bzero(m, name),
        "memchr" | "memrchr" => memchr(m, name),
        "strcpy" | "stpcpy" | "__strcpy_chk" | "__stpcpy_chk" => strcpy(m, name),
        "strncpy" | "__strncpy_chk" => strncpy(m, name),
        "strcat" | "strncat" | "__strcat_chk" | "__strncat_chk" => strcat(m, name),
        "strchr" | "strrchr" | "index" | "rindex" => strchr(m, name),
        "strstr" => strstr(m, name),
        "strdup" | "strndup" | "_strdup" => strdup(m, name),
        "malloc" => malloc(m, name),
        "calloc" => calloc(m, name),
        "realloc" => realloc(m, name),
        "free" => free(m, name),
        "puts" => puts(m, name),
        "putchar" | "putc" | "fputc" | "_IO_putc" | "putchar_unlocked" | "putc_unlocked" | "fputc_unlocked" => {
            putc(m, name)
        }
        "fputs" | "fputs_unlocked" => fputs(m, name),
        "fwrite" | "fwrite_unlocked" => fwrite(m, name),
        "fflush" | "setvbuf" | "setbuf" => Ok((0, String::new(), Some("0".into()))),
        "printf" | "fprintf" | "sprintf" | "snprintf" | "__printf_chk" | "__fprintf_chk" | "__sprintf_chk"
        | "__snprintf_chk" | "dprintf" => printf(m, name),
        "atoi" | "atol" | "atoll" | "strtol" | "strtoll" | "strtoul" | "strtoull" | "__isoc23_strtol"
        | "__isoc23_strtoul" | "__isoc23_strtoll" | "__isoc23_strtoull" => strtol(m, name),
        "abs" | "labs" | "llabs" => abs(m, name),
        "toupper" | "tolower" | "isdigit" | "isalpha" | "isalnum" | "isspace" | "isupper" | "islower" | "isxdigit"
        | "isprint" | "ispunct" | "iscntrl" | "isgraph" => ctype_fn(m, name),
        "__ctype_b_loc" | "__ctype_toupper_loc" | "__ctype_tolower_loc" => ctype_loc(m, name),
        "exit" | "_exit" | "_Exit" => a(m, 0, name).and_then(|c| Err(Stop::Exit(c as i32 as i64))),
        "abort" => Err(Stop::Abort("the program called abort()".into())),
        "__stack_chk_fail" | "__chk_fail" | "__fortify_fail" => Err(Stop::Abort(
            "stack smashing: the canary at fs:0x28 changed before the function returned (__stack_chk_fail)".into(),
        )),
        "__assert_fail" => assert_fail(m, name),
        _ => return None,
    };
    Some(r)
}

// ---- helpers ----

fn lib(call: &str, fault: Fault) -> Stop {
    Stop::LibFault { call: call.to_string(), fault }
}

fn a(m: &Machine, i: usize, call: &str) -> Result<u64, Stop> {
    m.arg(i).map_err(|f| lib(call, f))
}

fn cstr(m: &Machine, p: u64, max: usize, call: &str) -> Result<Vec<u8>, Stop> {
    m.mem.cstr(p, max).map(|(b, _)| b).map_err(|f| lib(call, f))
}

fn read(m: &Machine, p: u64, n: u64, call: &str) -> Result<Vec<u8>, Stop> {
    let n = big(n, call)?;
    let mut b = vec![0; n];
    m.mem.read(p, &mut b).map_err(|f| lib(call, f))?;
    Ok(b)
}

fn write(m: &mut Machine, p: u64, b: &[u8], call: &str) -> Result<(), Stop> {
    m.mem.write(p, b).map_err(|f| lib(call, f))
}

/// A length a stub will copy or compare, refused past a bound so one call cannot eat the machine.
fn big(n: u64, call: &str) -> Result<usize, Stop> {
    if n > MAX_LIB_BYTES {
        return Err(Stop::Abort(format!(
            "{call} was asked for {n:#x} bytes; nexo's stubs stop at {MAX_LIB_BYTES:#x} (a wrong length or a negative one?)"
        )));
    }
    Ok(n as usize)
}

const STR_MAX: usize = 1 << 20;

fn ok_num(v: u64, args: String) -> Answer {
    Ok((v, args, Some((v as i64).to_string())))
}

fn ok_ptr(m: &Machine, v: u64, args: String) -> Answer {
    Ok((v, args, Some(if v == 0 { "NULL".into() } else { m.show_ptr(v) })))
}

// ---- strings and memory ----

fn strlen(m: &mut Machine, call: &str) -> Answer {
    let p = a(m, 0, call)?;
    let max = if call == "strnlen" { a(m, 1, call)?.min(STR_MAX as u64) as usize } else { STR_MAX };
    let s = cstr(m, p, max, call)?;
    ok_num(s.len() as u64, m.show_ptr(p))
}

fn strcmp(m: &mut Machine, call: &str) -> Answer {
    let (x, y) = (a(m, 0, call)?, a(m, 1, call)?);
    let n = if call.contains('n') && call != "strcoll" { a(m, 2, call)?.min(STR_MAX as u64) as usize } else { STR_MAX };
    let (mut s1, mut s2) = (cstr(m, x, n, call)?, cstr(m, y, n, call)?);
    if call.contains("case") || call.contains("icmp") {
        s1.make_ascii_lowercase();
        s2.make_ascii_lowercase();
    }
    // compare with the terminating NUL, as C does
    let (k1, k2) = (s1.iter().chain(&[0]).copied(), s2.iter().chain(&[0]).copied());
    let r = k1.zip(k2).take(n).find(|(p, q)| p != q).map(|(p, q)| p as i64 - q as i64).unwrap_or(0);
    ok_num(r as i32 as u32 as u64, format!("{}, {}", m.show_ptr(x), m.show_ptr(y)))
        .map(|(v, s, _)| (v, s, Some(r.to_string())))
}

fn memcmp(m: &mut Machine, call: &str) -> Answer {
    let (x, y, n) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?);
    let (b1, b2) = (read(m, x, n, call)?, read(m, y, n, call)?);
    let r = b1.iter().zip(&b2).find(|(p, q)| p != q).map(|(&p, &q)| p as i64 - q as i64).unwrap_or(0);
    Ok((r as i32 as u32 as u64, format!("{x:#x}, {y:#x}, {n}"), Some(r.to_string())))
}

fn memcpy(m: &mut Machine, call: &str) -> Answer {
    let (d, s, n) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?);
    if call.ends_with("_chk") && n > a(m, 3, call)? {
        return Err(Stop::Abort(format!("{call}: {n} bytes into a buffer of {} (buffer overflow)", a(m, 3, call)?)));
    }
    let b = read(m, s, n, call)?;
    write(m, d, &b, call)?;
    let r = if call.contains("mempcpy") { d + n } else { d };
    Ok((r, format!("{d:#x}, {}, {n}", m.show_ptr(s)), None))
}

fn memset(m: &mut Machine, call: &str) -> Answer {
    let (d, c, n) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?);
    let b = vec![c as u8; big(n, call)?];
    write(m, d, &b, call)?;
    Ok((d, format!("{d:#x}, {:#x}, {n}", c as u8), None))
}

fn bzero(m: &mut Machine, call: &str) -> Answer {
    let (d, n) = (a(m, 0, call)?, a(m, 1, call)?);
    let b = vec![0; big(n, call)?];
    write(m, d, &b, call)?;
    Ok((0, format!("{d:#x}, {n}"), Some(String::new())))
}

fn memchr(m: &mut Machine, call: &str) -> Answer {
    let (p, c, n) = (a(m, 0, call)?, a(m, 1, call)? as u8, a(m, 2, call)?);
    let b = read(m, p, n, call)?;
    let at = if call == "memrchr" { b.iter().rposition(|&x| x == c) } else { b.iter().position(|&x| x == c) };
    ok_ptr(m, at.map(|i| p + i as u64).unwrap_or(0), format!("{p:#x}, {c:#x}, {n}"))
}

fn strcpy(m: &mut Machine, call: &str) -> Answer {
    let (d, s) = (a(m, 0, call)?, a(m, 1, call)?);
    let mut b = cstr(m, s, STR_MAX, call)?;
    let len = b.len() as u64;
    b.push(0);
    if call.ends_with("_chk") && b.len() as u64 > a(m, 2, call)? {
        return Err(Stop::Abort(format!(
            "{call}: {} bytes into a buffer of {} (buffer overflow)",
            b.len(),
            a(m, 2, call)?
        )));
    }
    write(m, d, &b, call)?;
    let r = if call.contains("stpcpy") { d + len } else { d };
    Ok((r, format!("{d:#x}, {}", m.show_ptr(s)), None))
}

fn strncpy(m: &mut Machine, call: &str) -> Answer {
    let (d, s, n) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?);
    let n = big(n, call)?;
    let mut b = cstr(m, s, n, call)?;
    b.resize(n, 0);
    write(m, d, &b, call)?;
    Ok((d, format!("{d:#x}, {}, {n}", m.show_ptr(s)), None))
}

fn strcat(m: &mut Machine, call: &str) -> Answer {
    let (d, s) = (a(m, 0, call)?, a(m, 1, call)?);
    let n = if call.contains("ncat") { a(m, 2, call)?.min(STR_MAX as u64) as usize } else { STR_MAX };
    let end = d + cstr(m, d, STR_MAX, call)?.len() as u64;
    let mut b = cstr(m, s, n, call)?;
    b.push(0);
    write(m, end, &b, call)?;
    Ok((d, format!("{d:#x}, {}", m.show_ptr(s)), None))
}

fn strchr(m: &mut Machine, call: &str) -> Answer {
    let (p, c) = (a(m, 0, call)?, a(m, 1, call)? as u8);
    let mut s = cstr(m, p, STR_MAX, call)?;
    s.push(0);
    let at = if call.contains('r') { s.iter().rposition(|&x| x == c) } else { s.iter().position(|&x| x == c) };
    ok_ptr(m, at.map(|i| p + i as u64).unwrap_or(0), format!("{}, {:?}", m.show_ptr(p), c as char))
}

fn strstr(m: &mut Machine, call: &str) -> Answer {
    let (p, q) = (a(m, 0, call)?, a(m, 1, call)?);
    let (h, n) = (cstr(m, p, STR_MAX, call)?, cstr(m, q, STR_MAX, call)?);
    let at = if n.is_empty() { Some(0) } else { h.windows(n.len()).position(|w| w == n.as_slice()) };
    ok_ptr(m, at.map(|i| p + i as u64).unwrap_or(0), format!("{}, {}", m.show_ptr(p), m.show_ptr(q)))
}

fn strdup(m: &mut Machine, call: &str) -> Answer {
    let p = a(m, 0, call)?;
    let n = if call == "strndup" { a(m, 1, call)?.min(STR_MAX as u64) as usize } else { STR_MAX };
    let mut b = cstr(m, p, n, call)?;
    b.push(0);
    let Some(d) = m.alloc(b.len() as u64) else { return ok_ptr(m, 0, m.show_ptr(p)) };
    write(m, d, &b, call)?;
    ok_ptr(m, d, m.show_ptr(p))
}

// ---- heap ----

fn malloc(m: &mut Machine, call: &str) -> Answer {
    let n = a(m, 0, call)?;
    let p = if n > MAX_LIB_BYTES { None } else { m.alloc(n) };
    ok_ptr(m, p.unwrap_or(0), n.to_string())
}

fn calloc(m: &mut Machine, call: &str) -> Answer {
    let (k, n) = (a(m, 0, call)?, a(m, 1, call)?);
    let p = match k.checked_mul(n) {
        Some(t) if t <= MAX_LIB_BYTES => m.alloc(t),
        _ => None,
    };
    ok_ptr(m, p.unwrap_or(0), format!("{k}, {n}"))
}

/// The live block that starts at `p`, or the reason `p` cannot be freed or resized.
fn block(m: &Machine, p: u64, call: &str) -> Result<Block, Stop> {
    match m.blocks.get(&p) {
        Some(b) if b.freed => Err(Stop::Abort(format!(
            "{call}({p:#x}): heap block #{} was already freed (double free or use after free)",
            b.n
        ))),
        Some(b) => Ok(b.clone()),
        None => Err(Stop::Abort(format!("{call}({p:#x}): not a pointer malloc returned ({})", m.place(p)))),
    }
}

fn free(m: &mut Machine, call: &str) -> Answer {
    let p = a(m, 0, call)?;
    if p != 0 {
        block(m, p, call)?;
        if let Some(b) = m.blocks.get_mut(&p) {
            b.freed = true;
        }
    }
    Ok((0, format!("{p:#x}"), Some(String::new())))
}

fn realloc(m: &mut Machine, call: &str) -> Answer {
    let (p, n) = (a(m, 0, call)?, a(m, 1, call)?);
    if p == 0 {
        return malloc(m, call);
    }
    let old = block(m, p, call)?;
    let Some(d) = (n <= MAX_LIB_BYTES).then(|| m.alloc(n)).flatten() else {
        return ok_ptr(m, 0, format!("{p:#x}, {n}"));
    };
    let b = read(m, p, old.size.min(n), call)?;
    write(m, d, &b, call)?;
    if let Some(b) = m.blocks.get_mut(&p) {
        b.freed = true;
    }
    ok_ptr(m, d, format!("{p:#x}, {n}"))
}

// ---- output ----

fn to_err(fp: u64) -> Option<bool> {
    if fp == file_value(1) {
        Some(false)
    } else if fp == file_value(2) {
        Some(true)
    } else {
        None
    }
}

fn stream(fp: u64, call: &str) -> Result<bool, Stop> {
    to_err(fp).ok_or_else(|| {
        Stop::Abort(format!("{call} to a FILE {fp:#x} that is not stdout or stderr (nexo stubs only those two)"))
    })
}

fn puts(m: &mut Machine, call: &str) -> Answer {
    let p = a(m, 0, call)?;
    let mut b = cstr(m, p, STR_MAX, call)?;
    b.push(b'\n');
    m.emit(false, &b);
    ok_num(b.len() as u64, m.show_ptr(p))
}

fn putc(m: &mut Machine, call: &str) -> Answer {
    let c = a(m, 0, call)? as u8;
    let err = if call.starts_with("putchar") { false } else { stream(a(m, 1, call)?, call)? };
    m.emit(err, &[c]);
    ok_num(c as u64, format!("{:?}", c as char))
}

fn fputs(m: &mut Machine, call: &str) -> Answer {
    let (p, fp) = (a(m, 0, call)?, a(m, 1, call)?);
    let err = stream(fp, call)?;
    let b = cstr(m, p, STR_MAX, call)?;
    m.emit(err, &b);
    ok_num(1, m.show_ptr(p))
}

fn fwrite(m: &mut Machine, call: &str) -> Answer {
    let (p, size, n, fp) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?, a(m, 3, call)?);
    let err = stream(fp, call)?;
    let b = read(m, p, size.saturating_mul(n), call)?;
    m.emit(err, &b);
    ok_num(n, format!("{p:#x}, {size}, {n}"))
}

fn printf(m: &mut Machine, call: &str) -> Answer {
    // where the format is, and where the output goes
    let (fmt_at, dest) = match call {
        "printf" => (0, Dest::Out(false)),
        "__printf_chk" => (1, Dest::Out(false)),
        "fprintf" => (1, Dest::Out(stream(a(m, 0, call)?, call)?)),
        "__fprintf_chk" => (2, Dest::Out(stream(a(m, 0, call)?, call)?)),
        "dprintf" => (1, Dest::Out(a(m, 0, call)? == 2)),
        "sprintf" => (1, Dest::Buf(a(m, 0, call)?, None)),
        "__sprintf_chk" => (3, Dest::Buf(a(m, 0, call)?, Some(a(m, 2, call)?))),
        "snprintf" => (2, Dest::Buf(a(m, 0, call)?, Some(a(m, 1, call)?))),
        _ => (4, Dest::Buf(a(m, 0, call)?, Some(a(m, 1, call)?))), // __snprintf_chk
    };
    let f = a(m, fmt_at, call)?;
    let fmt = cstr(m, f, STR_MAX, call)?;
    let mut cur = Cursor::new(m.ms_abi, fmt_at + 1);
    let out = format(m, &fmt, &mut cur, call)?;
    let shown = m.show_ptr(f);
    match dest {
        Dest::Out(err) => m.emit(err, &out),
        Dest::Buf(d, cap) => {
            let mut b = out.clone();
            if let Some(cap) = cap {
                if call.starts_with("__sprintf") && b.len() as u64 >= cap {
                    return Err(Stop::Abort(format!(
                        "{call}: {} bytes into a buffer of {cap} (buffer overflow)",
                        b.len() + 1
                    )));
                }
                b.truncate(cap.saturating_sub(1) as usize);
            }
            if cap != Some(0) {
                b.push(0);
                write(m, d, &b, call)?;
            }
        }
    }
    ok_num(out.len() as u64, shown)
}

enum Dest {
    Out(bool),
    Buf(u64, Option<u64>),
}

/// Walks the variable arguments of a call: integers and floats take separate registers in System V and
/// share the stack after them; Microsoft x64 has one position for both.
struct Cursor {
    ms: bool,
    int: usize,
    float: usize,
    stack: usize,
}

impl Cursor {
    fn new(ms: bool, first: usize) -> Cursor {
        let regs = if ms { 4 } else { 6 };
        Cursor { ms, int: first.min(regs), float: 0, stack: first.saturating_sub(regs) }
    }

    fn int(&mut self, m: &Machine, call: &str) -> Result<u64, Stop> {
        let regs = if self.ms { 4 } else { 6 };
        if self.int < regs {
            self.int += 1;
            return a(m, self.int - 1, call);
        }
        self.stack += 1;
        a(m, regs + self.stack - 1, call)
    }

    fn float(&mut self, m: &Machine, call: &str) -> Result<f64, Stop> {
        if self.ms {
            return self.int(m, call).map(f64::from_bits);
        }
        if self.float < 8 {
            self.float += 1;
            return Ok(f64::from_bits(m.cpu.xmm[self.float - 1] as u64));
        }
        self.stack += 1;
        a(m, 6 + self.stack - 1, call).map(f64::from_bits)
    }
}

/// C's printf formatting, for the conversions small programs use.
fn format(m: &Machine, fmt: &[u8], cur: &mut Cursor, call: &str) -> Result<Vec<u8>, Stop> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            out.push(fmt[i]);
            i += 1;
            continue;
        }
        i += 1;
        let (mut left, mut plus, mut space, mut alt, mut zero) = (false, false, false, false, false);
        while let Some(&c) = fmt.get(i) {
            match c {
                b'-' => left = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alt = true,
                b'0' => zero = true,
                _ => break,
            }
            i += 1;
        }
        let mut width = 0usize;
        if fmt.get(i) == Some(&b'*') {
            let w = cur.int(m, call)? as i32;
            left |= w < 0;
            width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            while let Some(d) = fmt.get(i).filter(|c| c.is_ascii_digit()) {
                width = width * 10 + (d - b'0') as usize;
                i += 1;
            }
        }
        let mut prec: Option<usize> = None;
        if fmt.get(i) == Some(&b'.') {
            i += 1;
            if fmt.get(i) == Some(&b'*') {
                let p = cur.int(m, call)? as i32;
                prec = (p >= 0).then_some(p as usize);
                i += 1;
            } else {
                let mut p = 0;
                while let Some(d) = fmt.get(i).filter(|c| c.is_ascii_digit()) {
                    p = p * 10 + (d - b'0') as usize;
                    i += 1;
                }
                prec = Some(p);
            }
        }
        width = width.min(4096);
        let prec = prec.map(|p| p.min(4096));
        let mut bits = 32;
        while let Some(&c) = fmt.get(i) {
            match c {
                b'h' => bits = if bits == 16 { 8 } else { 16 },
                b'l' | b'z' | b'j' | b't' | b'q' | b'L' => bits = 64,
                _ => break,
            }
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        let pad = |out: &mut Vec<u8>, prefix: &[u8], body: &[u8], numeric: bool| {
            let len = prefix.len() + body.len();
            let fill = width.saturating_sub(len);
            if left {
                out.extend_from_slice(prefix);
                out.extend_from_slice(body);
                out.extend(std::iter::repeat_n(b' ', fill));
            } else if zero && numeric {
                out.extend_from_slice(prefix);
                out.extend(std::iter::repeat_n(b'0', fill));
                out.extend_from_slice(body);
            } else {
                out.extend(std::iter::repeat_n(b' ', fill));
                out.extend_from_slice(prefix);
                out.extend_from_slice(body);
            }
        };
        let sign = |neg: bool| -> &'static [u8] {
            if neg {
                b"-"
            } else if plus {
                b"+"
            } else if space {
                b" "
            } else {
                b""
            }
        };
        match conv {
            b'%' => out.push(b'%'),
            b'd' | b'i' => {
                let v = cur.int(m, call)?;
                let v = match bits {
                    8 => v as i8 as i64,
                    16 => v as i16 as i64,
                    32 => v as i32 as i64,
                    _ => v as i64,
                };
                let body = digits(v.unsigned_abs(), 10, false, prec);
                pad(&mut out, sign(v < 0), &body, prec.is_none());
            }
            b'u' | b'x' | b'X' | b'o' => {
                let v = cur.int(m, call)? & if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
                let base = match conv {
                    b'u' => 10,
                    b'o' => 8,
                    _ => 16,
                };
                let mut body = digits(v, base, conv == b'X', prec);
                let prefix: &[u8] = match conv {
                    b'x' if alt && v != 0 => b"0x",
                    b'X' if alt && v != 0 => b"0X",
                    _ => b"",
                };
                if conv == b'o' && alt && body.first() != Some(&b'0') {
                    body.insert(0, b'0');
                }
                pad(&mut out, prefix, &body, prec.is_none());
            }
            b'c' => {
                let c = cur.int(m, call)? as u8;
                pad(&mut out, b"", &[c], false);
            }
            b's' => {
                let p = cur.int(m, call)?;
                let s = if p == 0 { b"(null)".to_vec() } else { cstr(m, p, prec.unwrap_or(STR_MAX), call)? };
                pad(&mut out, b"", &s, false);
            }
            b'p' => {
                let p = cur.int(m, call)?;
                if p == 0 {
                    pad(&mut out, b"", b"(nil)", false);
                } else {
                    pad(&mut out, b"0x", &digits(p, 16, false, None), false);
                }
            }
            b'f' | b'F' | b'e' | b'E' | b'g' | b'G' => {
                let v = cur.float(m, call)?;
                let body = float(v.abs(), conv, prec.unwrap_or(6), alt);
                pad(&mut out, sign(v.is_sign_negative() && !v.is_nan()), body.as_bytes(), v.is_finite());
            }
            b'n' => {
                cur.int(m, call)?;
            }
            other => {
                return Err(Stop::Abort(format!(
                    "{call}: format conversion %{} is not one nexo's printf stub knows",
                    other as char
                )))
            }
        }
    }
    Ok(out)
}

fn digits(v: u64, base: u64, upper: bool, prec: Option<usize>) -> Vec<u8> {
    let set: &[u8] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
    let mut d = Vec::new();
    let mut x = v;
    while x > 0 {
        d.push(set[(x % base) as usize]);
        x /= base;
    }
    let min = prec.unwrap_or(1);
    while d.len() < min {
        d.push(b'0');
    }
    d.reverse();
    d
}

/// `%f`, `%e` and `%g` of a value that is not negative.
fn float(v: f64, conv: u8, prec: usize, alt: bool) -> String {
    let upper = conv.is_ascii_uppercase();
    let s = if v.is_nan() {
        "nan".to_string()
    } else if v.is_infinite() {
        "inf".to_string()
    } else {
        match conv.to_ascii_lowercase() {
            b'f' => format!("{v:.prec$}"),
            b'e' => exp(v, prec),
            _ => {
                let p = prec.max(1);
                let x = if v == 0.0 { 0 } else { exp(v, p - 1).rsplit('e').next().unwrap().parse::<i32>().unwrap() };
                let mut s = if x < -4 || x >= p as i32 {
                    exp(v, p - 1)
                } else {
                    format!("{v:.*}", (p as i32 - 1 - x) as usize)
                };
                if !alt {
                    let (num, tail) = match s.find('e') {
                        Some(k) => (s[..k].to_string(), s[k..].to_string()),
                        None => (s.clone(), String::new()),
                    };
                    let num = if num.contains('.') {
                        num.trim_end_matches('0').trim_end_matches('.').to_string()
                    } else {
                        num
                    };
                    s = num + &tail;
                }
                s
            }
        }
    };
    if upper {
        s.to_uppercase()
    } else {
        s
    }
}

/// `1.500000e+02` from `1.5e2`.
fn exp(v: f64, prec: usize) -> String {
    let s = format!("{v:.prec$e}");
    let (mant, e) = s.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    format!("{mant}e{}{:02}", if e < 0 { '-' } else { '+' }, e.unsigned_abs())
}

// ---- numbers and characters ----

fn strtol(m: &mut Machine, call: &str) -> Answer {
    let p = a(m, 0, call)?;
    let to = call.trim_start_matches("__isoc23_");
    let (end_at, base) = if to.starts_with("ato") { (0, 10) } else { (a(m, 1, call)?, a(m, 2, call)? as u32) };
    let s = cstr(m, p, 4096, call)?;
    let mut i = s.iter().take_while(|c| c.is_ascii_whitespace()).count();
    let neg = s.get(i) == Some(&b'-');
    if matches!(s.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut base = base;
    let hex = s.get(i) == Some(&b'0') && matches!(s.get(i + 1), Some(b'x' | b'X'));
    if (base == 0 || base == 16) && hex && s.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) {
        i += 2;
        base = 16;
    } else if base == 0 {
        base = if s.get(i) == Some(&b'0') { 8 } else { 10 };
    }
    let start = i;
    let mut v: u128 = 0;
    while let Some(d) = s.get(i).and_then(|&c| (c as char).to_digit(36)).filter(|&d| d < base) {
        v = (v * base as u128 + d as u128).min(u64::MAX as u128 + 1);
        i += 1;
    }
    let used = if i == start { 0 } else { i };
    let signed = !to.contains("ul");
    let r: u64 = if signed {
        let x = if neg { -(v as i128) } else { v as i128 };
        x.clamp(i64::MIN as i128, i64::MAX as i128) as i64 as u64
    } else if v > u64::MAX as u128 {
        u64::MAX
    } else if neg {
        (v as u64).wrapping_neg()
    } else {
        v as u64
    };
    let r = if to == "atoi" { r as i32 as i64 as u64 } else { r };
    if end_at != 0 {
        write(m, end_at, &(p + used as u64).to_le_bytes(), call)?;
    }
    let shown = if signed { (r as i64).to_string() } else { r.to_string() };
    Ok((r, m.show_ptr(p), Some(shown)))
}

fn abs(m: &mut Machine, call: &str) -> Answer {
    let v = a(m, 0, call)?;
    let r = if call == "abs" { (v as i32).wrapping_abs() as u32 as u64 } else { (v as i64).wrapping_abs() as u64 };
    ok_num(r, (v as i64).to_string())
}

/// glibc's class bits as `__ctype_b_loc` gives them (little-endian order of `_ISbit`).
fn class_bits(c: u8) -> u16 {
    let ch = c as char;
    let mut b = 0;
    let mut on = |bit: u16, yes: bool| {
        if yes {
            b |= bit
        }
    };
    on(0x100, ch.is_ascii_uppercase());
    on(0x200, ch.is_ascii_lowercase());
    on(0x400, ch.is_ascii_alphabetic());
    on(0x800, ch.is_ascii_digit());
    on(0x1000, ch.is_ascii_hexdigit());
    on(0x2000, matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'));
    on(0x4000, (0x20..0x7f).contains(&c));
    on(0x8000, (0x21..0x7f).contains(&c));
    on(0x1, c == b' ' || c == b'\t');
    on(0x2, c < 0x20 || c == 0x7f);
    on(0x4, ch.is_ascii_punctuation());
    on(0x8, ch.is_ascii_alphanumeric());
    b
}

fn ctype_fn(m: &mut Machine, call: &str) -> Answer {
    let v = a(m, 0, call)? as i32;
    let c = (0..=255).contains(&v).then_some(v as u8);
    let bit = match call {
        "toupper" => return ok_num(c.map(|c| c.to_ascii_uppercase() as u64).unwrap_or(v as u32 as u64), v.to_string()),
        "tolower" => return ok_num(c.map(|c| c.to_ascii_lowercase() as u64).unwrap_or(v as u32 as u64), v.to_string()),
        "isupper" => 0x100,
        "islower" => 0x200,
        "isalpha" => 0x400,
        "isdigit" => 0x800,
        "isxdigit" => 0x1000,
        "isspace" => 0x2000,
        "isprint" => 0x4000,
        "isgraph" => 0x8000,
        "iscntrl" => 0x2,
        "ispunct" => 0x4,
        _ => 0x8,
    };
    let r = c.map(|c| class_bits(c) & bit != 0).unwrap_or(false) as u64;
    ok_num(r, v.to_string())
}

/// `__ctype_b_loc()` and its kin return a pointer to a pointer into a table indexed from -128 to 255.
fn ctype_loc(m: &mut Machine, call: &str) -> Answer {
    if !m.mem.is_mapped(CTYPE_BASE) {
        m.mem.map(CTYPE_BASE, 3 * PAGE, false, false);
        let mut b = Vec::with_capacity(384 * 2);
        let mut up = Vec::with_capacity(384 * 4);
        let mut low = Vec::with_capacity(384 * 4);
        for i in -128i32..256 {
            let c = (i as u8, i >= 0);
            b.extend_from_slice(&(if c.1 { class_bits(c.0) } else { 0 }).to_le_bytes());
            let (u, l) = if c.1 { (c.0.to_ascii_uppercase() as i32, c.0.to_ascii_lowercase() as i32) } else { (i, i) };
            up.extend_from_slice(&u.to_le_bytes());
            low.extend_from_slice(&l.to_le_bytes());
        }
        let ptrs = [CTYPE_BASE + 0x100 + 256, CTYPE_BASE + PAGE + 512, CTYPE_BASE + 2 * PAGE + 512];
        let mut cells = Vec::new();
        for p in ptrs {
            cells.extend_from_slice(&p.to_le_bytes());
        }
        let _ = m.mem.load(CTYPE_BASE, &cells);
        let _ = m.mem.load(CTYPE_BASE + 0x100, &b);
        let _ = m.mem.load(CTYPE_BASE + PAGE, &up);
        let _ = m.mem.load(CTYPE_BASE + 2 * PAGE, &low);
    }
    let k = match call {
        "__ctype_b_loc" => 0,
        "__ctype_toupper_loc" => 1,
        _ => 2,
    };
    Ok((CTYPE_BASE + 8 * k, String::new(), None))
}

fn assert_fail(m: &mut Machine, call: &str) -> Answer {
    let (e, f, line, func) = (a(m, 0, call)?, a(m, 1, call)?, a(m, 2, call)?, a(m, 3, call)?);
    let text = |p| cstr(m, p, 256, call).map(|b| String::from_utf8_lossy(&b).into_owned());
    Err(Stop::Abort(format!("assertion failed: {} ({}:{} in {})", text(e)?, text(f)?, line as u32, text(func)?)))
}
