//! The gdb dialect: what a model types from habit (`x/20bx $rsp`, `printf "%d\n", n`, `set $v = ...`, a script with
//! `commands ... end`, `run args` last) and how it maps to the debugger's own commands. Only text work lives here;
//! the commands that act on a session are in `cmds/gdb.rs`.

/// A gdb command name as the debugger's own name, or the name itself when it needs no change.
pub fn alias(name: &str) -> &str {
    match name {
        "backtrace" | "where" => "bt",
        "disassemble" | "disas" | "disass" | "disassem" => "dis",
        "tb" | "tbr" => "tbreak",
        "br" | "brea" | "bre" => "break",
        "si" | "stepi" => "step",
        "ni" | "nexti" => "next",
        "fin" => "finish",
        "f" => "frame",
        "u" | "advance" => "until",
        "i" => "info",
        "d" => "delete",
        "inspect" => "print",
        "r" => "run",
        "q" => "quit",
        "k" => "kill",
        "disp" => "display",
        other => other,
    }
}

/// The command word without a `/FMT` suffix (`x/20bx` is `x`), mapped through [`alias`].
pub fn base_name(first: &str) -> &str {
    alias(first.split('/').next().unwrap_or(first))
}

/// Settings that mean nothing to this debugger (no pager, no prompts): they are accepted and dropped.
pub fn is_noise_setting(words: &[String]) -> bool {
    matches!(
        words.get(1).map(String::as_str),
        Some(
            "pagination"
                | "confirm"
                | "width"
                | "height"
                | "debuginfod"
                | "print"
                | "p"
                | "style"
                | "logging"
                | "verbose"
                | "breakpoint"
                | "disassembly-flavor"
                | "startup-with-shell"
                | "listsize"
                | "step-mode"
                | "language"
                | "can-use-hw-watchpoints"
                | "unwindonsignal"
                | "max-value-size"
                | "backtrace"
                | "osabi"
                | "auto-load"
                | "history"
        )
    )
}

/// What a `set ...` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetKind {
    /// A debugger setting that has no meaning here.
    Noise,
    /// `set $name = expr` for a name that is not a register: a convenience variable.
    Var,
    /// `set $reg = expr` or `set var $reg = expr`: writes a register.
    Register(String, String),
    /// `set args ...`.
    Args(Vec<String>),
    /// `set var x = ...`, `set {int}addr = ...`: changes the program.
    Memory,
    /// `set environment NAME=VALUE` (or `NAME VALUE`): part of how the program starts.
    Env(String, String),
    /// `set cwd DIR`: the folder the program starts in.
    Cwd(String),
    /// `set disable-randomization on|off`: whether the program starts with address randomisation off.
    Randomization { off: bool },
    /// `set follow-fork-mode parent|child`, `set detach-on-fork on|off`: the `follow` setting it stands for
    /// (`detach` for `detach-on-fork on`: `both` becomes `parent`, the rest stays).
    Follow(&'static str),
}

/// `NAME=VALUE` or `NAME VALUE` after `set environment`; a name alone is an empty value, as in gdb.
fn env_pair(rest: &[String]) -> Option<(String, String)> {
    let first = rest.first()?;
    if let Some((k, v)) = first.split_once('=') {
        let tail: Vec<&str> = rest[1..].iter().map(String::as_str).collect();
        let v = if tail.is_empty() { v.to_string() } else { format!("{v} {}", tail.join(" ")) };
        return (!k.is_empty()).then(|| (k.to_string(), v));
    }
    match rest.get(1).map(String::as_str) {
        Some("=") => Some((first.clone(), rest[2..].join(" "))),
        _ => Some((first.clone(), rest[1..].join(" "))),
    }
}

/// Classify the words of a `set` command; `is_reg` says whether a name is a register.
pub fn set_kind(words: &[String], is_reg: &dyn Fn(&str) -> bool) -> SetKind {
    if words.get(1).is_some_and(|w| w == "args") {
        return SetKind::Args(words[2..].to_vec());
    }
    match words.get(1).map(String::as_str) {
        Some("environment" | "env") => {
            if let Some((k, v)) = env_pair(&words[2..]) {
                return SetKind::Env(k, v);
            }
        }
        Some("cwd") if words.len() > 2 => return SetKind::Cwd(words[2..].join(" ")),
        Some("disable-randomization") => {
            let off = !matches!(words.get(2).map(String::as_str), Some("off" | "0" | "no" | "disable"));
            return SetKind::Randomization { off };
        }
        // gdb holds a forked child stopped with `detach-on-fork off`: here that child is a session of its own
        Some("follow-fork-mode") => match words.get(2).map(String::as_str) {
            Some("child") => return SetKind::Follow("child"),
            Some("parent") => return SetKind::Follow("parent"),
            _ => {}
        },
        Some("detach-on-fork") => {
            let off = matches!(words.get(2).map(String::as_str), Some("off" | "0" | "no"));
            // `on` is gdb's default: it undoes `off` and leaves `follow-fork-mode child` as it is
            return SetKind::Follow(if off { "both" } else { "detach" });
        }
        _ => {}
    }
    if is_noise_setting(words) {
        return SetKind::Noise;
    }
    let at = if words.get(1).is_some_and(|w| w == "var" || w == "variable") { 2 } else { 1 };
    let text = words[at.min(words.len())..].join(" ");
    let Some((lhs, rhs)) = split_assign(&text) else { return SetKind::Memory };
    let lhs = lhs.trim();
    match lhs.strip_prefix('$') {
        Some(n) if !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_') => {
            if is_reg(n) {
                SetKind::Register(n.to_string(), rhs.trim().to_string())
            } else {
                SetKind::Var
            }
        }
        _ => SetKind::Memory,
    }
}

/// `a = b` at the first `=` that is not part of `==`, `<=`, `>=`, `!=`.
fn split_assign(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'=' {
            let prev = if i > 0 { b[i - 1] } else { b' ' };
            let next = b.get(i + 1).copied().unwrap_or(b' ');
            if next != b'=' && !matches!(prev, b'=' | b'<' | b'>' | b'!') {
                return Some((&s[..i], &s[i + 1..]));
            }
            return None;
        }
    }
    None
}

/// The parts of a script: cut at `;` and at line ends outside quotes, trimmed, without empty parts and without `#`
/// comment lines. A double quote opens a span anywhere (left open it keeps the rest as one part). A single quote
/// opens one only at a word start (after whitespace, `=`, `(`, `,` or `[`, or at the start) and only when it closes,
/// so `';'` and `'a;b'` stay whole while the apostrophe of `it's` or an unclosed `'` is a plain character.
pub fn units(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut cur) = (Vec::new(), String::new());
    let mut push = |cur: &mut String| {
        let t = cur.trim();
        if !t.is_empty() && !t.starts_with('#') {
            out.push(t.to_string());
        }
        cur.clear();
    };
    // the end of a span opened at `i` with `open`, backslash escapes skipped
    let close = |i: usize, open: char| -> Option<usize> {
        let mut j = i + 1;
        while j < chars.len() {
            match chars[j] {
                '\\' => j += 1,
                c if c == open => return Some(j),
                _ => {}
            }
            j += 1;
        }
        None
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' => {
                let word_start =
                    i == 0 || cur.is_empty() || matches!(chars[i - 1], c if c.is_whitespace() || "=(,[".contains(c));
                let end = if c == '"' || word_start { close(i, c) } else { None };
                match end {
                    Some(e) => {
                        cur.extend(&chars[i..=e]);
                        i = e;
                    }
                    None if c == '"' => {
                        cur.extend(&chars[i..]);
                        i = chars.len();
                    }
                    None => cur.push(c),
                }
            }
            '#' if cur.trim().is_empty() => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            ';' | '\n' => push(&mut cur),
            '\r' => {}
            _ => cur.push(c),
        }
        i += 1;
    }
    push(&mut cur);
    out
}

/// Cut at commas outside quotes and brackets: the arguments of a `printf`.
pub fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut q) = (Vec::new(), String::new(), 0i32, false);
    let mut prev = ' ';
    for c in s.chars() {
        match c {
            '"' if prev != '\\' => {
                q = !q;
                cur.push(c);
            }
            '(' | '[' | '{' if !q => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' if !q => {
                depth -= 1;
                cur.push(c);
            }
            ',' if !q && depth <= 0 => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
        prev = c;
    }
    out.push(cur.trim().to_string());
    out
}

/// `\n`, `\t`, `\\`, `\"`, `\xNN` and octal escapes of a gdb string.
pub fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\x07'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('x') => {
                let mut n = 0u32;
                let mut k = 0;
                while k < 2 {
                    match it.peek().and_then(|c| c.to_digit(16)) {
                        Some(d) => {
                            n = n * 16 + d;
                            it.next();
                            k += 1;
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(n).unwrap_or('?'));
            }
            Some(d @ '0'..='7') => {
                let mut n = d.to_digit(8).unwrap_or(0);
                let mut k = 1;
                while k < 3 {
                    match it.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) => {
                            n = n * 8 + d;
                            it.next();
                            k += 1;
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(n).unwrap_or('?'));
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A piece of a `printf` format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Lit(String),
    Conv(Conv),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conv {
    pub spec: char,
    pub left: bool,
    pub zero: bool,
    pub plus: bool,
    pub alt: bool,
    pub width: usize,
    pub prec: Option<usize>,
    /// `l`, `ll`, `z`, `j`, `t`: 64 bits.
    pub wide: bool,
    /// `hh` or `h`.
    pub short: u8,
}

/// The pieces of an already unescaped format: text and conversions (`%d`, `%08x`, `%-4s`, `%lu`, `%p`, `%%`).
pub fn parse_format(fmt: &str) -> Result<Vec<Piece>, String> {
    let c: Vec<char> = fmt.chars().collect();
    let (mut out, mut lit, mut i) = (Vec::new(), String::new(), 0);
    while i < c.len() {
        if c[i] != '%' {
            lit.push(c[i]);
            i += 1;
            continue;
        }
        i += 1;
        if c.get(i) == Some(&'%') {
            lit.push('%');
            i += 1;
            continue;
        }
        let mut v = Conv {
            spec: 'd',
            left: false,
            zero: false,
            plus: false,
            alt: false,
            width: 0,
            prec: None,
            wide: false,
            short: 0,
        };
        while let Some(&f) = c.get(i) {
            match f {
                '-' => v.left = true,
                '0' => v.zero = true,
                '+' => v.plus = true,
                '#' => v.alt = true,
                ' ' => {}
                _ => break,
            }
            i += 1;
        }
        while let Some(d) = c.get(i).and_then(|c| c.to_digit(10)) {
            v.width = v.width * 10 + d as usize;
            i += 1;
        }
        if c.get(i) == Some(&'.') {
            i += 1;
            let mut p = 0;
            while let Some(d) = c.get(i).and_then(|c| c.to_digit(10)) {
                p = p * 10 + d as usize;
                i += 1;
            }
            v.prec = Some(p);
        }
        while let Some(&m) = c.get(i) {
            match m {
                'l' | 'z' | 'j' | 't' | 'q' | 'L' => v.wide = true,
                'h' => v.short += 1,
                _ => break,
            }
            i += 1;
        }
        let Some(&s) = c.get(i) else { return Err("the format ends inside a `%` conversion".into()) };
        if !matches!(s, 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'c' | 's' | 'p' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G') {
            return Err(format!("`%{s}` is not a conversion printf knows (d i u x X o c s p f e g)"));
        }
        v.spec = if s == 'i' { 'd' } else { s };
        i += 1;
        if !lit.is_empty() {
            out.push(Piece::Lit(std::mem::take(&mut lit)));
        }
        out.push(Piece::Conv(v));
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

/// How many arguments a format takes.
pub fn conversions(pieces: &[Piece]) -> usize {
    pieces.iter().filter(|p| matches!(p, Piece::Conv(_))).count()
}

/// The format and the arguments of `printf "fmt", a, b` when the words came from `split_words` (the quotes are
/// already gone, a comma may be stuck to the format). `rest` is the words after `printf` joined with spaces. The
/// format is the shortest start that takes as many arguments as there are after it.
pub fn printf_parts(rest: &str) -> Result<(String, Vec<String>), String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("usage: printf \"format\", arg, ...".into());
    }
    // the quotes may still be there when the text comes straight from a script
    if let Some(body) = rest.strip_prefix('"') {
        let mut esc = false;
        for (i, ch) in body.char_indices() {
            match ch {
                '\\' if !esc => esc = true,
                '"' if !esc => {
                    let fmt = &body[..i];
                    let tail = body[i + 1..].trim();
                    let args = match tail.strip_prefix(',') {
                        Some(a) => split_args(a),
                        None if tail.is_empty() => Vec::new(),
                        None => return Err("after the format comes `, arg, ...`".into()),
                    };
                    return Ok((fmt.to_string(), args));
                }
                _ => esc = false,
            }
        }
        return Err("the format string has no closing quote".into());
    }
    let parts = split_args(rest);
    for j in 0..parts.len() {
        let fmt = parts[..=j].join(", ");
        let args = &parts[j + 1..];
        if let Ok(p) = parse_format(&unescape(&fmt)) {
            if conversions(&p) == args.len() {
                return Ok((fmt, args.to_vec()));
            }
        }
    }
    Err(format!(
        "the format needs a different number of arguments than the {} given: printf \"%d %s\\n\", n, p",
        parts.len().saturating_sub(1)
    ))
}

/// Apply width and fill to text.
fn pad(c: &Conv, s: String, numeric: bool) -> String {
    let n = s.chars().count();
    if n >= c.width {
        return s;
    }
    let fill = c.width - n;
    if c.left {
        format!("{s}{}", " ".repeat(fill))
    } else if c.zero && numeric {
        // zeros go after a sign or a 0x prefix
        let (head, body) = if let Some(b) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            (&s[..2], b)
        } else if let Some(b) = s.strip_prefix('-').or_else(|| s.strip_prefix('+')) {
            (&s[..1], b)
        } else {
            ("", s.as_str())
        };
        format!("{head}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{s}", " ".repeat(fill))
    }
}

/// An integer conversion of the 64-bit value `v`.
pub fn render_int(c: &Conv, v: u64) -> String {
    let bits = if c.wide {
        64
    } else if c.short == 1 {
        16
    } else if c.short >= 2 {
        8
    } else {
        32
    };
    let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let u = v & mask;
    let signed = {
        let sh = 64 - bits;
        ((u << sh) as i64) >> sh
    };
    let mut s = match c.spec {
        'd' => {
            let t = signed.to_string();
            if c.plus && signed >= 0 {
                format!("+{t}")
            } else {
                t
            }
        }
        'u' => u.to_string(),
        'x' => format!("{}{u:x}", if c.alt && u != 0 { "0x" } else { "" }),
        'X' => format!("{}{u:X}", if c.alt && u != 0 { "0X" } else { "" }),
        'o' => format!("{}{u:o}", if c.alt { "0" } else { "" }),
        'c' => char::from_u32((v & 0xff) as u32).map(String::from).unwrap_or_default(),
        'p' => {
            if v == 0 {
                "(nil)".to_string()
            } else {
                format!("{v:#x}")
            }
        }
        _ => v.to_string(),
    };
    if let Some(p) = c.prec.filter(|_| matches!(c.spec, 'd' | 'u' | 'x' | 'X' | 'o')) {
        let neg = s.starts_with('-');
        let digits = s.trim_start_matches('-').to_string();
        if digits.len() < p {
            s = format!("{}{}{digits}", if neg { "-" } else { "" }, "0".repeat(p - digits.len()));
        }
    }
    pad(c, s, !matches!(c.spec, 'c' | 'p'))
}

/// A string conversion.
pub fn render_str(c: &Conv, text: &str) -> String {
    let cut: String = match c.prec {
        Some(p) => text.chars().take(p).collect(),
        None => text.to_string(),
    };
    pad(c, cut, false)
}

/// A floating conversion.
pub fn render_float(c: &Conv, v: f64) -> String {
    let p = c.prec.unwrap_or(6);
    let s = match c.spec {
        'e' | 'E' => format!("{v:.p$e}"),
        'g' | 'G' => format!("{v}"),
        _ => format!("{v:.p$}"),
    };
    pad(c, s, true)
}

/// What a gdb `x/FMT` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XFmt {
    pub count: usize,
    pub fmt: char,
    /// Bytes per unit.
    pub size: usize,
}

/// Parse `20bx`, `s`, `4gx`, `3i`, `-4x` (the text after the slash). Letters: x d u o t a c f s i z; sizes b h w g.
pub fn parse_xfmt(spec: &str) -> Result<XFmt, String> {
    let digits: String = spec.chars().take_while(|c| c.is_ascii_digit()).collect();
    let count = if digits.is_empty() { 1 } else { digits.parse::<usize>().map_err(|_| "the count is too big")? };
    let (mut fmt, mut size) = (None, None);
    for ch in spec[digits.len()..].chars() {
        match ch {
            'x' | 'd' | 'u' | 'o' | 't' | 'a' | 'c' | 'f' | 's' | 'i' | 'z' => fmt = Some(ch),
            'b' => size = Some(1),
            'h' => size = Some(2),
            'w' => size = Some(4),
            'g' => size = Some(8),
            other => return Err(format!("{other:?} is not an x/FMT letter: x d u o t a c f s i z and sizes b h w g")),
        }
    }
    let fmt = fmt.unwrap_or('x');
    let size = size.unwrap_or(match fmt {
        'a' => 8,
        'c' | 's' | 'i' => 1,
        'f' => 8,
        _ => 4,
    });
    Ok(XFmt { count: count.max(1), fmt, size })
}

/// One number as `x/FMT` shows it.
pub fn show_unit(fmt: char, size: usize, v: u64) -> String {
    let bits = (size * 8) as u32;
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let u = v & mask;
    let signed = {
        let sh = 64 - bits.min(64);
        ((u << sh) as i64) >> sh
    };
    match fmt {
        'd' => signed.to_string(),
        'u' => u.to_string(),
        'o' => format!("0{u:o}"),
        't' => format!("{u:0w$b}", w = bits as usize),
        'c' => {
            let b = (u & 0xff) as u8;
            let shown = match b {
                b'\n' => "\\n".to_string(),
                b'\t' => "\\t".to_string(),
                b'\r' => "\\r".to_string(),
                0 => "\\000".to_string(),
                0x20..=0x7e => (b as char).to_string(),
                _ => format!("\\{b:03o}"),
            };
            format!("{} '{shown}'", signed as i8)
        }
        'f' => match size {
            4 => format!("{}", f32::from_bits(u as u32)),
            8 => format!("{}", f64::from_bits(u)),
            _ => signed.to_string(),
        },
        'z' => format!("0x{u:0w$x}", w = size * 2),
        _ => format!("0x{u:0w$x}", w = size * 2),
    }
}

/// Where `x/FMT` rows break: units per line, as gdb does.
pub fn per_line(fmt: char, size: usize) -> usize {
    match (fmt, size) {
        ('c', _) => 8,
        ('t', 1) => 8,
        ('t', 2) => 8,
        ('t', 4) => 4,
        ('t', 8) => 2,
        (_, 1) => 8,
        (_, 2) => 8,
        (_, 4) => 4,
        _ => 2,
    }
}

/// gdb commands this debugger has no twin for, with what to type instead.
pub fn hint(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "list" | "l" => "`dis [addr] [N]` shows the code; a stop shows file:line when the program has DWARF lines",
        "return" | "jump" => "`setreg rip <addr>` moves the program counter (asks); `finish` runs to the caller",
        "rwatch" | "awatch" => "`watch <addr|A..B> [len] r|w|x` stops on a read, a write or a fetch (`if`, `after` and `log` as on `break`)",
        "handle" => "`catch add <SIG>` or `catch del <SIG>` choose the signals that stop the program; `signal <SIG|0>` continues delivering one (0 drops the pending one)",
        "source" => "scripts go in the call itself, one command per line, or `a; b; c`",
        "shell" | "!" => "the debug tool does not run shell commands: use the shell tool",
        "target" | "tui" | "layout" | "focus" | "refresh" | "winheight" => {
            "this debugger has no screen modes or targets: `run`, `attach <pid>` and `connect` start or join a program"
        }
        "file" | "exec-file" | "symbol-file" | "directory" | "cd" | "pwd" | "tty" => {
            "the program is chosen by `run <path> [args]`"
        }
        "start" | "starti" => "`run --main <path> [args]` starts and stops at main",
        "reverse-step" | "reverse-stepi" | "reverse-next" | "reverse-nexti" | "reverse-continue" | "reverse-finish"
        | "rs" | "rsi" | "rn" | "rni" | "rc" => {
            "this debugger looks back with `back N`, `back --reg R`, `back --mem A`; it does not undo (`record` starts the record)"
        }
        "checkpoint" | "restart" => "this debugger cannot go back in time: `cont` forward, `history` lists past stops",
        "quit" => "`end` as its own call closes the session",
        "kill" => "`kill` ends the program, `end` closes the session",
        "define" | "document" | "python" | "py" | "guile" => {
            "no scripting language here: put the commands in the call, `a; b; c`"
        }
        _ => return None,
    })
}

/// What gdb's `thread apply all bt` becomes here.
pub const THREAD_APPLY: &str = "`threads --bt N` lists every thread with its name, state, system call and its own backtrace of at most N frames (gdb's `thread apply all bt`); `thread <tid>` then `bt` reads one";

/// What stands for gdb's `run` once the session is started: run on, and wait for a whole run.
pub const RUN_CONT: &str = "cont --wait 30";

/// One part of a gdb script that starts the program: how to start it and what to run before and after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The `run` command that starts the session (stops at the entry point).
    pub start: String,
    /// The commands to run after the start, in the order gdb would have run them, with `cont` where `run` was.
    pub rest: Vec<String>,
    /// Settings left out because this debugger has no use for them.
    pub dropped: usize,
    /// The script ended with `quit`: the session is closed after it.
    pub end: bool,
}

/// A script with `run` after other commands is gdb's shape: breakpoints and `commands` first, `run` last. The
/// program is started (stopped at its entry), the first part runs, then `cont` stands for `run`. `None` when the
/// script is already in the debugger's own shape (`run` first, or no `run` at all). `Err` when `run` comes after other
/// commands and no program is named.
pub fn plan(parts: &[String], is_reg: &dyn Fn(&str) -> bool) -> Result<Option<Plan>, String> {
    let word = |p: &str| p.split_whitespace().next().unwrap_or("").to_string();
    let Some(at) = parts.iter().position(|p| matches!(word(p).as_str(), "run" | "r" | "start")) else {
        return Ok(None);
    };
    let (mut exe, mut args): (Option<String>, Vec<String>) = (None, Vec::new());
    let (mut rest, mut dropped, mut end) = (Vec::new(), 0usize, false);
    let mut flags: Vec<String> = Vec::new();
    let sink = |p: &str, exe: &mut Option<String>, args: &mut Vec<String>, flags: &mut Vec<String>| -> bool {
        let w = crate::split_words(p);
        match w.first().map(String::as_str) {
            Some("unset") if w.len() == 3 && matches!(w[1].as_str(), "environment" | "env") => {
                flags.extend(["--unset".to_string(), w[2].clone()]);
                true
            }
            Some("file" | "exec-file") if w.len() == 2 => {
                *exe = Some(w[1].clone());
                true
            }
            Some("set") => match set_kind(&w, is_reg) {
                SetKind::Args(a) => {
                    *args = a;
                    true
                }
                SetKind::Env(k, v) => {
                    flags.extend(["--env".to_string(), format!("{k}={v}")]);
                    true
                }
                SetKind::Cwd(d) => {
                    flags.extend(["--cwd".to_string(), d]);
                    true
                }
                SetKind::Randomization { off } => {
                    flags.retain(|f| f != "--aslr");
                    if !off {
                        flags.push("--aslr".to_string());
                    }
                    true
                }
                SetKind::Noise => true,
                _ => false,
            },
            _ => false,
        }
    };
    for p in &parts[..at] {
        let w = crate::split_words(p);
        let noise = w.first().is_some_and(|f| f == "set") && is_noise_setting(&w);
        if sink(p, &mut exe, &mut args, &mut flags) {
            dropped += usize::from(noise);
        } else {
            rest.push(p.clone());
        }
    }
    if at == 0 {
        return Ok(None);
    }
    let run = crate::split_words(&parts[at]);
    let to_main = run[0] == "start";
    let mut run_args: Vec<String> = run[1..].to_vec();
    // the debugger's own launch options may follow `run`: they join the ones the script set
    while let Some(f) = run_args.first().cloned() {
        let take = match f.as_str() {
            "--main" | "--aslr" => 1,
            "--env" | "--unset" | "--cwd" | "--argv0" if run_args.len() > 1 => 2,
            _ => break,
        };
        flags.extend(run_args.drain(..take));
    }
    let own_flag = flags.iter().any(|f| f == "--main");
    flags.retain(|f| f != "--main");
    let exe = match exe {
        Some(e) => e,
        // without `file`, the program is the first word of `run` (the debugger's own shape)
        None if run_args.first().is_some_and(|w| !w.starts_with('-')) => run_args.remove(0),
        None => {
            return Err(
                "the script starts the program with `run` but names no program: put `file <path>` before it (or write `run <path> args` first)"
                    .into(),
            )
        }
    };
    if run_args.is_empty() {
        run_args = args;
    }
    let quote =
        |w: &str| if w.contains(char::is_whitespace) || w.is_empty() { format!("\"{w}\"") } else { w.to_string() };
    let tail: Vec<String> = run_args.iter().map(|w| quote(w)).collect();
    let mut lead = String::new();
    if to_main || own_flag {
        lead.push_str(" --main");
    }
    for f in &flags {
        lead.push(' ');
        lead.push_str(&quote(f));
    }
    let start = format!(
        "run{lead} {}{}",
        quote(&exe),
        if tail.is_empty() { String::new() } else { format!(" {}", tail.join(" ")) }
    );
    if !(to_main || own_flag) {
        rest.push(RUN_CONT.to_string());
    }
    for p in &parts[at + 1..] {
        let w = crate::split_words(p);
        if w.first().is_some_and(|f| f == "set") && is_noise_setting(&w) {
            dropped += 1;
        } else if matches!(word(p).as_str(), "quit" | "q") {
            end = true;
        } else {
            rest.push(p.clone());
        }
    }
    Ok(Some(Plan { start, rest, dropped, end }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn reg(n: &str) -> bool {
        matches!(n, "rax" | "rbp" | "rip" | "rsp")
    }

    #[test]
    fn aliases_and_slash_formats_map_to_the_debuggers_names() {
        assert_eq!(base_name("x/20bx"), "x");
        assert_eq!(base_name("p/x"), "p");
        assert_eq!(base_name("backtrace"), "bt");
        assert!(THREAD_APPLY.contains("threads --bt N"));
        assert_eq!(base_name("disassemble"), "dis");
        assert_eq!(base_name("si"), "step");
        assert_eq!(base_name("ni"), "next");
        assert_eq!(base_name("tb"), "tbreak");
        assert_eq!(base_name("mem"), "mem");
    }

    #[test]
    fn scripts_split_at_semicolons_and_lines_outside_quotes() {
        let t = "set pagination off\n# a comment\nbreak *0x10; printf \"a;b %d\\n\", 3\r\ncommands\nsilent\nend\n";
        assert_eq!(
            units(t),
            ["set pagination off", "break *0x10", "printf \"a;b %d\\n\", 3", "commands", "silent", "end"]
        );
        assert_eq!(units(""), Vec::<String>::new());
    }

    #[test]
    fn arguments_split_at_top_level_commas_only() {
        assert_eq!(
            split_args("*(void**)$v, *(void**)($v+8), f(1, 2), \"a,b\""),
            ["*(void**)$v", "*(void**)($v+8)", "f(1, 2)", "\"a,b\""]
        );
    }

    #[test]
    fn a_format_and_its_arguments_are_found_with_or_without_the_quotes() {
        // as split_words leaves it: quotes gone, the comma stuck to the format
        let (f, a) = printf_parts("begin=%p end=%p\\n, *(void**)$v, *(void**)($v+8)").unwrap();
        assert_eq!(f, "begin=%p end=%p\\n");
        assert_eq!(a, ["*(void**)$v", "*(void**)($v+8)"]);
        // a comma inside the format
        let (f, a) = printf_parts("n=%d, m=%d\\n, 1, 2").unwrap();
        assert_eq!((f.as_str(), a.len()), ("n=%d, m=%d\\n", 2));
        // with the quotes
        let (f, a) = printf_parts("\"x %s\\n\", $k").unwrap();
        assert_eq!((f.as_str(), a), ("x %s\\n", vec!["$k".to_string()]));
        let (f, a) = printf_parts("\"just text\\n\"").unwrap();
        assert_eq!((f.as_str(), a.len()), ("just text\\n", 0));
        assert!(printf_parts("%d %d\\n, 1").is_err());
        assert!(printf_parts("").is_err());
    }

    #[test]
    fn formats_render_like_c() {
        let p = parse_format(&unescape("a=%d b=%08x c=%-4s| %lu %c %p %%\\n")).unwrap();
        assert_eq!(conversions(&p), 6);
        let convs: Vec<Conv> =
            p.iter().filter_map(|x| if let Piece::Conv(c) = x { Some(c.clone()) } else { None }).collect();
        assert_eq!(render_int(&convs[0], 0xffff_ffff), "-1");
        assert_eq!(render_int(&convs[1], 0x2f), "0000002f");
        assert_eq!(render_str(&convs[2], "ab"), "ab  ");
        assert_eq!(render_int(&convs[3], u64::MAX), u64::MAX.to_string());
        assert_eq!(render_int(&convs[4], 0x41), "A");
        assert_eq!(render_int(&convs[5], 0), "(nil)");
        assert_eq!(render_int(&convs[5], 0x7ffe_0000), "0x7ffe0000");
        assert_eq!(unescape("a\\n\\t\\\\\\x41\\101"), "a\n\t\\AA");
        assert!(parse_format("%q").is_err() || parse_format("%y").is_err());
    }

    #[test]
    fn x_formats_parse_with_gdbs_defaults() {
        assert_eq!(parse_xfmt("20bx").unwrap(), XFmt { count: 20, fmt: 'x', size: 1 });
        assert_eq!(parse_xfmt("s").unwrap(), XFmt { count: 1, fmt: 's', size: 1 });
        assert_eq!(parse_xfmt("4gx").unwrap(), XFmt { count: 4, fmt: 'x', size: 8 });
        assert_eq!(parse_xfmt("xg").unwrap().size, 8);
        assert_eq!(parse_xfmt("3i").unwrap().count, 3);
        assert_eq!(parse_xfmt("").unwrap(), XFmt { count: 1, fmt: 'x', size: 4 });
        assert!(parse_xfmt("5q").is_err());
        assert_eq!(show_unit('x', 1, 0x3), "0x03");
        assert_eq!(show_unit('x', 4, 0x31), "0x00000031");
        assert_eq!(show_unit('d', 1, 0xff), "-1");
        assert_eq!(show_unit('u', 1, 0xff), "255");
        assert_eq!(show_unit('c', 1, 0x30), "48 '0'");
        assert_eq!(show_unit('t', 1, 5), "00000101");
        assert_eq!(per_line('x', 1), 8);
        assert_eq!(per_line('x', 4), 4);
        assert_eq!(per_line('x', 8), 2);
    }

    #[test]
    fn set_tells_settings_variables_registers_and_memory_apart() {
        let k = |l: &str| set_kind(&crate::split_words(l), &reg);
        assert_eq!(k("set pagination off"), SetKind::Noise);
        assert_eq!(k("set confirm off"), SetKind::Noise);
        assert_eq!(k("set debuginfod enabled off"), SetKind::Noise);
        assert_eq!(k("set $v = (void*)($rbp-0x310)"), SetKind::Var);
        assert_eq!(k("set $k=*(void**)($rbp-0x378)"), SetKind::Var);
        assert_eq!(k("set $rax = 5"), SetKind::Register("rax".into(), "5".into()));
        assert_eq!(k("set var $rip = main"), SetKind::Register("rip".into(), "main".into()));
        assert_eq!(k("set var x = 1"), SetKind::Memory);
        assert_eq!(k("set {int}0x1000 = 1"), SetKind::Memory);
        assert_eq!(k("set args -e t.bin"), SetKind::Args(s(&["-e", "t.bin"])));
        assert_eq!(k("set environment A=1"), SetKind::Env("A".into(), "1".into()));
        assert_eq!(k("set environment A 1 2"), SetKind::Env("A".into(), "1 2".into()));
        assert_eq!(k("set env A = 1"), SetKind::Env("A".into(), "1".into()));
        assert_eq!(k("set cwd /tmp"), SetKind::Cwd("/tmp".into()));
        assert_eq!(k("set follow-fork-mode child"), SetKind::Follow("child"));
        assert_eq!(k("set detach-on-fork off"), SetKind::Follow("both"));
        assert_eq!(k("set detach-on-fork on"), SetKind::Follow("detach"));
        assert_eq!(k("set disable-randomization off"), SetKind::Randomization { off: false });
        assert_eq!(k("set disable-randomization"), SetKind::Randomization { off: true });
        assert_eq!(k("set $a == 1"), SetKind::Memory);
    }

    #[test]
    fn a_gdb_script_with_run_last_becomes_start_then_the_rest_then_cont() {
        let parts = units(
            "set pagination off\nbreak *0x10\ncommands\nsilent\nprintf \"x\\n\"\ncontinue\nend\nrun -e t.bin -p 0\nx/4x $rsp\nquit",
        );
        assert!(plan(&parts, &reg).is_err(), "a run with no program named is told so");
        let mut with_file = vec!["file ./prog".to_string()];
        with_file.extend(parts.clone());
        let p = plan(&with_file, &reg).unwrap().unwrap();
        assert_eq!(p.start, "run ./prog -e t.bin -p 0");
        assert_eq!(
            p.rest,
            ["break *0x10", "commands", "silent", "printf \"x\\n\"", "continue", "end", RUN_CONT, "x/4x $rsp"]
        );
        assert_eq!(p.dropped, 1);
        assert!(p.end);
        // the run carries its own program in the debugger's shape, with set args
        let q = units("break main\nset args a b\nrun ./p");
        let p = plan(&q, &reg).unwrap().unwrap();
        assert_eq!(p.start, "run ./p a b");
        assert_eq!(p.rest, ["break main", RUN_CONT]);
        // `run` first is the debugger's own shape: left alone
        assert_eq!(plan(&units("run ./p; cont"), &reg), Ok(None));
        assert_eq!(plan(&units("break main; cont"), &reg), Ok(None));
        // start stops at main and does not continue
        let p = plan(&units("file ./p\nbreak helper\nstart"), &reg).unwrap().unwrap();
        assert_eq!(p.start, "run --main ./p");
        assert_eq!(p.rest, ["break helper"]);
    }

    #[test]
    fn single_quotes_keep_a_semicolon_whole() {
        assert_eq!(units("print c == ';'; cont"), s(&["print c == ';'", "cont"]));
        assert_eq!(units("find --grep 'a;b'; fn main"), s(&["find --grep 'a;b'", "fn main"]));
        assert_eq!(units("print '\\''; cont"), s(&["print '\\''", "cont"]));
        // a double quote inside single quotes, and an apostrophe inside double quotes, open nothing
        assert_eq!(units("print '\"'; a"), s(&["print '\"'", "a"]));
        assert_eq!(units("note \"it's\"; b"), s(&["note \"it's\"", "b"]));
        // an apostrophe in a comment line does not swallow the lines after it
        assert_eq!(units("# don't\nrun"), s(&["run"]));
    }

    #[test]
    fn an_apostrophe_in_prose_and_an_unclosed_single_quote_are_plain_characters() {
        assert_eq!(units("note 0x401000 the user's key; xref 0x401000").len(), 2);
        assert_eq!(units("echo don't; cont"), s(&["echo don't", "cont"]));
        assert_eq!(units("a; find 'x; b"), s(&["a", "find 'x", "b"]));
        assert_eq!(units("print c=='x'; print c=='\\''; d"), s(&["print c=='x'", "print c=='\\''", "d"]));
        // a double quote left open still keeps the rest as one part
        assert_eq!(units("a; say \"x; b"), s(&["a", "say \"x; b"]));
    }

    #[test]
    fn record_is_a_command_and_the_reverse_ones_say_what_to_type() {
        // `record` has its own command now: no hint stands in its place
        assert_eq!(hint("record"), None);
        assert_eq!(base_name("record"), "record");
        for c in ["reverse-step", "reverse-stepi", "reverse-next", "reverse-continue", "reverse-finish", "rc"] {
            let h = hint(c).unwrap_or_else(|| panic!("{c} has no hint"));
            assert!(h.contains("back N") && h.contains("back --reg R") && h.contains("back --mem A"), "{c}: {h}");
            assert!(h.contains("does not undo"), "{c}: {h}");
        }
        assert!(hint("checkpoint").unwrap().contains("cannot go back in time"));
    }
}
