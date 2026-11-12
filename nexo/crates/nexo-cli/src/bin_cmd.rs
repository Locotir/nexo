//! `nexo bin`: read an ELF or PE x86-64 file as bytes and answer in bounded pieces.
//! The file is never run or loaded. See the `nexo-bin` crate.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use nexo_bin::{ElemType, Error, FnQuery, FuncSort, Image, Output, StrSort, DEFAULT_LINES, MIN_LINES};

#[derive(Args, Debug)]
pub struct BinArgs {
    #[command(subcommand)]
    pub op: BinOp,
}

/// Options every op takes.
#[derive(Args, Debug)]
pub struct Common {
    /// Most lines in the answer (at least 10). When it is cut, the last line gives the command for the next piece.
    #[arg(long, default_value_t = DEFAULT_LINES, value_name = "N")]
    pub lines: usize,
    /// Start at this address (or line position); the cut line prints the value to use.
    #[arg(long, value_parser = parse_addr, value_name = "ADDR")]
    pub from: Option<u64>,
    /// Print the answer as JSON (same bound).
    #[arg(long)]
    pub json: bool,
    /// Print on stderr how long the references took to build and how long the whole answer took.
    #[arg(long)]
    pub timing: bool,
}

#[derive(Subcommand, Debug)]
pub enum BinOp {
    /// Format, entry, sections, counts and what the headers say about hardening.
    Info {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// The map of the file in one call: main, imports by capability, top strings and functions, next commands.
    Overview {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Known crypto constants (AES, SHA, MD5, CRC-32, ChaCha, base64) with evidence and the functions that use them.
    Crypto {
        file: PathBuf,
        /// Name the tables and note the functions that use them (never replaces a name or note).
        #[arg(long)]
        save: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Backward slice: the instructions of a function that produced a value read at an address.
    Slice {
        file: PathBuf,
        /// The instruction whose inputs to follow (an address inside a function).
        at: String,
        /// Follow this register (rax, edi, r8, ...) instead of what the instruction reads.
        #[arg(long)]
        reg: Option<String>,
        /// For a call: follow argument N (1 is the first).
        #[arg(long)]
        arg: Option<usize>,
        /// Follow a stack slot, like rbp-0x14 or rsp+8.
        #[arg(long)]
        mem: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// How one function reaches another: the shortest call chains, with the call site of each hop.
    Path {
        file: PathBuf,
        /// Start: an address or a function name.
        from_fn: String,
        /// Goal: an address, a function or an import name.
        to_fn: String,
        /// Also follow functions whose address is taken (possible calls through pointers).
        #[arg(long)]
        ptrs: bool,
        /// How many shortest routes to list (1 to 20).
        #[arg(long, default_value_t = 3)]
        routes: usize,
        #[command(flatten)]
        common: Common,
    },
    /// Callers (--up) or callees of a function to N levels, as an indented tree.
    Tree {
        file: PathBuf,
        /// An address or a function name.
        target: String,
        /// Show who calls it instead of what it calls.
        #[arg(long)]
        up: bool,
        /// Levels to expand (1 to 12).
        #[arg(long, default_value_t = 3)]
        depth: usize,
        /// Also count functions whose address is taken.
        #[arg(long)]
        ptrs: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Disassemble the whole function that holds an address, or has a name (Intel syntax).
    Fn {
        file: PathBuf,
        /// An address (0x401a30) or a function name.
        target: String,
        /// Show the instruction bytes.
        #[arg(long)]
        bytes: bool,
        /// Show everything from the target up to this address, across functions (padding folded).
        #[arg(long, value_name = "ADDR")]
        to: Option<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Pseudo-C of one or more functions (lift, flow, structure; cached by file hash).
    Decomp {
        file: PathBuf,
        /// Addresses (0x401a30) or function names.
        #[arg(required = true)]
        targets: Vec<String>,
        /// Start every line with the address of the instruction it came from.
        #[arg(long)]
        addr: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Give a function a C prototype, so decomp names and types its arguments (`--clear` removes it).
    Proto {
        file: PathBuf,
        /// An address inside the function or its name.
        target: String,
        /// For example: int load(const char *path, struct item *it)
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        text: Option<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Define a small struct for decomp: fields as offset:type:name (`--clear` removes it).
    Struct {
        file: PathBuf,
        name: String,
        /// For example: 0:i32:id 8:ptr:next
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        fields: Option<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Rename a variable of one decompiled function: a1, v3 or local_18 (`--clear` undoes it).
    Var {
        file: PathBuf,
        /// An address inside the function or its name.
        target: String,
        /// The variable as decomp prints it.
        key: String,
        /// The new name.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        name: Option<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Who calls, jumps to, reads, writes, takes the address of, or points at an address or name.
    Xref {
        file: PathBuf,
        /// An address (0x401a30) or a function, symbol or import name.
        #[arg(required_unless_present = "str_text")]
        target: Option<String>,
        /// Instead: the strings that contain this text, and the references to each.
        #[arg(long = "str", value_name = "TEXT", conflicts_with = "target")]
        str_text: Option<String>,
        /// With --str: ignore case.
        #[arg(long = "i", requires = "str_text")]
        insensitive: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Make library signatures from static libraries on this machine (libc.a, the Rust std), so stripped
    /// programs get their library functions named. Run once; `funcs`, `fn`, `overview` and `decomp` use the file.
    Sigs {
        /// Libraries (.a, .rlib) or object files; default: the ones found on this machine.
        libs: Vec<PathBuf>,
        /// Keep every function, not only the common ones (memory, string, malloc, stdio, Rust fmt and alloc).
        #[arg(long)]
        all: bool,
        /// Write here instead of `<data dir>/sigs/local.sig` (the folder `nexo bin` reads).
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
        #[command(flatten)]
        common: Common,
    },
    /// List the functions with size, callers and callees (default order: most callers first).
    Funcs {
        file: PathBuf,
        /// addr, size, callers or callees.
        #[arg(long, default_value = "callers", value_parser = FuncSort::NAMES)]
        sort: String,
        /// Keep the functions whose name matches this regular expression.
        #[arg(long, value_name = "REGEX")]
        grep: Option<String>,
        /// Keep the functions of at least this many bytes.
        #[arg(long, default_value_t = 0, value_name = "BYTES")]
        min_size: u64,
        #[command(flatten)]
        common: Common,
    },
    /// Who calls a function and what it calls, with the number of call sites.
    Calls {
        file: PathBuf,
        /// An address inside the function (0x401a30) or its name.
        target: String,
        #[command(flatten)]
        common: Common,
    },
    /// Give an address a name that stays with the file (`--clear` removes it).
    Name {
        file: PathBuf,
        /// An address or the name of a function, symbol or import.
        target: String,
        /// The new name: 1 to 64 characters of letters, digits and _ . : $ @ < > ~ -
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        name: Option<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        common: Common,
    },
    /// Leave a one-line note at an address that stays with the file (`--clear` removes it).
    Note {
        file: PathBuf,
        /// An address or the name of a function, symbol or import.
        target: String,
        /// The note (up to 200 characters).
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        text: Vec<String>,
        #[arg(long)]
        clear: bool,
        #[command(flatten)]
        common: Common,
    },
    /// List the names and notes this file has.
    Names {
        file: PathBuf,
        #[command(flatten)]
        common: Common,
    },
    /// Run one function (or code from an address) in an emulator, without running the program: args in,
    /// return value, memory written, library calls and a trace tail out.
    Emu {
        file: PathBuf,
        /// A function name, or an address to start at.
        target: String,
        /// Arguments in order: 42, -1, 0x401000, str:TEXT, hex:DE AD, buf:SIZE (a buffer to write into).
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
        /// Stop when execution reaches this address (run a stretch of code).
        #[arg(long, value_parser = parse_addr, value_name = "ADDR")]
        to: Option<u64>,
        /// Set a register before the run: --reg rbx=5 or --reg rsi=str:abc (repeat).
        #[arg(long = "reg", value_name = "NAME=VALUE")]
        regs: Vec<String>,
        /// Put bytes in the file's memory first: --mem 0x4010=hex:01 02, str:TEXT, u32:7 (repeat).
        #[arg(long = "mem", value_name = "ADDR=VALUE")]
        mems: Vec<String>,
        /// Make an import return NUMBER and do nothing else: --stub rand=4 (repeat).
        #[arg(long = "stub", value_name = "NAME=NUMBER")]
        stubs: Vec<String>,
        /// Calling convention: sysv or ms (default: ms for PE files, sysv for ELF).
        #[arg(long, value_name = "ABI")]
        abi: Option<String>,
        /// Most instructions to run.
        #[arg(long, default_value_t = nexo_bin::emu_limits::DEFAULT_STEPS, value_name = "N")]
        steps: u64,
        /// Most seconds to run.
        #[arg(long, default_value_t = nexo_bin::emu_limits::DEFAULT_SECS, value_name = "S")]
        secs: u64,
        /// Instructions in the trace tail (0 to 200).
        #[arg(long, default_value_t = nexo_bin::emu_limits::DEFAULT_TRACE, value_name = "N")]
        trace: usize,
        /// Bytes shown per written range (16 to 4096).
        #[arg(long, default_value_t = nexo_bin::emu_limits::DEFAULT_DUMP, value_name = "N")]
        dump: usize,
        /// Also show what was written on the stack.
        #[arg(long)]
        stack: bool,
        /// Show memory after the run: --show ADDR:LEN (repeat).
        #[arg(long, value_name = "ADDR:LEN")]
        show: Vec<String>,
        #[command(flatten)]
        common: Common,
    },
    /// Show LEN bytes at an address as a typed table.
    Bytes {
        file: PathBuf,
        #[arg(value_parser = parse_addr)]
        addr: u64,
        /// How many bytes (at most 262144).
        #[arg(value_parser = parse_addr)]
        len: u64,
        /// u8, u16, u32, u64, i32, f32, f64, ptr (shows what it points to) or str.
        #[arg(long = "as", default_value = "u8", value_parser = ElemType::NAMES)]
        ty: String,
        #[command(flatten)]
        common: Common,
    },
    /// List ASCII and UTF-16 strings with their addresses and references; --grep keeps the ones that match.
    Strings {
        file: PathBuf,
        /// A regular expression.
        #[arg(long, value_name = "REGEX")]
        grep: Option<String>,
        /// addr (default) or refs (most referenced first).
        #[arg(long, default_value = "addr", value_parser = StrSort::NAMES)]
        sort: String,
        #[command(flatten)]
        common: Common,
    },
    /// Find hex bytes (?? matches any byte) or, with --imm, a number used as an immediate or stored as data.
    Find {
        file: PathBuf,
        /// Hex bytes, like 48 8d 05.
        #[arg(required_unless_present = "imm", conflicts_with = "imm")]
        pattern: Vec<String>,
        /// A number (0x1f4 or 500) instead of bytes.
        #[arg(long, allow_hyphen_values = true, value_name = "NUMBER")]
        imm: Option<String>,
        #[command(flatten)]
        common: Common,
    },
}

fn parse_addr(s: &str) -> Result<u64, String> {
    nexo_bin::parse_addr(s).map_err(|e| e.to_string())
}

impl BinOp {
    fn file(&self) -> &Path {
        match self {
            BinOp::Info { file, .. }
            | BinOp::Overview { file, .. }
            | BinOp::Path { file, .. }
            | BinOp::Slice { file, .. }
            | BinOp::Crypto { file, .. }
            | BinOp::Tree { file, .. }
            | BinOp::Fn { file, .. }
            | BinOp::Bytes { file, .. }
            | BinOp::Strings { file, .. }
            | BinOp::Xref { file, .. }
            | BinOp::Funcs { file, .. }
            | BinOp::Decomp { file, .. }
            | BinOp::Proto { file, .. }
            | BinOp::Struct { file, .. }
            | BinOp::Var { file, .. }
            | BinOp::Calls { file, .. }
            | BinOp::Name { file, .. }
            | BinOp::Note { file, .. }
            | BinOp::Names { file, .. }
            | BinOp::Find { file, .. }
            | BinOp::Emu { file, .. } => file,
            BinOp::Sigs { .. } => Path::new(""),
        }
    }

    fn common(&self) -> &Common {
        match self {
            BinOp::Info { common, .. }
            | BinOp::Overview { common, .. }
            | BinOp::Path { common, .. }
            | BinOp::Slice { common, .. }
            | BinOp::Crypto { common, .. }
            | BinOp::Tree { common, .. }
            | BinOp::Fn { common, .. }
            | BinOp::Bytes { common, .. }
            | BinOp::Strings { common, .. }
            | BinOp::Xref { common, .. }
            | BinOp::Funcs { common, .. }
            | BinOp::Decomp { common, .. }
            | BinOp::Proto { common, .. }
            | BinOp::Struct { common, .. }
            | BinOp::Var { common, .. }
            | BinOp::Calls { common, .. }
            | BinOp::Name { common, .. }
            | BinOp::Note { common, .. }
            | BinOp::Names { common, .. }
            | BinOp::Find { common, .. }
            | BinOp::Sigs { common, .. }
            | BinOp::Emu { common, .. } => common,
        }
    }
}

/// Answer one question: the text (or JSON) to print. Names and notes live in nexo's data dir.
pub fn execute(a: &BinArgs) -> Result<String, Error> {
    execute_in(a, &nexo_core::paths::data_dir())
}

/// Like [`execute`], with the data dir given (tests use a temporary one).
pub fn execute_in(a: &BinArgs, data_dir: &Path) -> Result<String, Error> {
    let t0 = std::time::Instant::now();
    if let BinOp::Sigs { libs, all, out, .. } = &a.op {
        return make_sigs(libs, *all, out.as_deref(), data_dir);
    }
    if nexo_bin::is_wasm(a.op.file()) {
        return wasm_in(a);
    }
    let mut img = Image::open(a.op.file())?;
    let warning = nexo_bin::attach_names(&mut img, data_dir);
    let c = a.op.common();
    let (from, lines) = (c.from, c.lines.max(MIN_LINES));
    let out: Output = match &a.op {
        BinOp::Info { .. } => nexo_bin::info(&img),
        BinOp::Overview { .. } => nexo_bin::overview_window(&img, from, lines)?,
        BinOp::Crypto { save, .. } => nexo_bin::crypto_window(&img, save.then_some(data_dir), from, lines)?,
        BinOp::Slice { at, reg, arg, mem, .. } => {
            nexo_bin::slice_window(&img, at, reg.as_deref(), *arg, mem.as_deref(), from, lines)?
        }
        BinOp::Path { from_fn, to_fn, ptrs, routes, .. } => {
            nexo_bin::path_window(&img, from_fn, to_fn, *ptrs, *routes, from, lines)?
        }
        BinOp::Tree { target, up, depth, ptrs, .. } => {
            nexo_bin::tree_window(&img, target, *up, *depth, *ptrs, from, lines)?
        }
        BinOp::Fn { target, bytes, to, .. } => nexo_bin::func_view(
            &img,
            &FnQuery {
                to: to.as_deref(),
                with_bytes: *bytes,
                from,
                budget: lines,
                json: c.json,
                ..FnQuery::new(target)
            },
        )?,
        BinOp::Xref { target, str_text, insensitive, .. } => match (str_text, target) {
            (Some(t), _) => nexo_bin::xref_str_window(&img, t, *insensitive, from, lines)?,
            (None, Some(t)) => nexo_bin::xref_window(&img, t, from, lines)?,
            (None, None) => return Err(Error::Usage("give an address or a name, or --str TEXT".into())),
        },
        BinOp::Funcs { sort, grep, min_size, .. } => {
            nexo_bin::func_list_window(&img, FuncSort::parse(sort)?, grep.as_deref(), *min_size, from, lines)?
        }
        BinOp::Decomp { targets, addr, .. } => nexo_bin::decomp_window(&img, data_dir, targets, *addr, from, lines)?,
        BinOp::Proto { target, text, clear, .. } => {
            nexo_bin::proto_set(&img, data_dir, target, if *clear { None } else { text.as_deref() })?
        }
        BinOp::Struct { name, fields, clear, .. } => {
            nexo_bin::struct_set(&img, data_dir, name, if *clear { None } else { fields.as_deref() })?
        }
        BinOp::Var { target, key, name, clear, .. } => {
            nexo_bin::var_set(&img, data_dir, target, key, if *clear { None } else { name.as_deref() })?
        }
        BinOp::Calls { target, .. } => nexo_bin::calls_window(&img, target, from, lines)?,
        BinOp::Name { target, name, clear, .. } => match (clear, name) {
            (false, Some(n)) => nexo_bin::name_set(&img, data_dir, target, n)?,
            _ => nexo_bin::name_clear(&img, data_dir, target)?,
        },
        BinOp::Note { target, text, clear, .. } => {
            if *clear {
                nexo_bin::note_clear(&img, data_dir, target)?
            } else {
                nexo_bin::note_set(&img, data_dir, target, &text.join(" "))?
            }
        }
        BinOp::Sigs { .. } => unreachable!("handled before the file is opened"),
        BinOp::Names { .. } => nexo_bin::names_list_window(&img, from, lines),
        BinOp::Bytes { addr, len, ty, .. } => nexo_bin::bytes(&img, *addr, *len, ElemType::parse(ty)?)?,
        BinOp::Strings { grep, sort, .. } => {
            nexo_bin::strings_window(&img, grep.as_deref(), StrSort::parse(sort)?, from, lines)?
        }
        BinOp::Emu { target, args, to, regs, mems, stubs, abi, steps, secs, trace, dump, stack, show, .. } => {
            nexo_bin::emulate(
                &img,
                &nexo_bin::EmuSpec {
                    args: args.clone(),
                    to: *to,
                    regs: regs.clone(),
                    mem: mems.clone(),
                    stubs: stubs.clone(),
                    abi: abi.clone(),
                    steps: *steps,
                    secs: *secs,
                    trace: *trace,
                    dump: *dump,
                    stack: *stack,
                    show: show.clone(),
                    ..nexo_bin::EmuSpec::new(target)
                },
            )?
        }
        BinOp::Find { pattern, imm, .. } => match imm {
            Some(n) => nexo_bin::find_imm(&img, nexo_bin::parse_number(n)?)?,
            None => nexo_bin::find_bytes(&img, &pattern.join(" "))?,
        },
    };
    let page = out.page_as(from, lines, c.json);
    if c.timing {
        let built = img.refs().build.as_secs_f64() * 1000.0;
        eprintln!(
            "timing: references built in {built:.1} ms; whole answer {:.1} ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }
    if c.json {
        let mut value = page.value;
        if let (Some(w), Some(o)) = (&warning, value.as_object_mut()) {
            o.insert("warning".into(), serde_json::Value::String(w.clone()));
        }
        serde_json::to_string_pretty(&value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        let mut text = page.text();
        if let Some(w) = warning {
            text.push('\n');
            text.push_str(&w);
        }
        Ok(text)
    }
}

/// The ops that read a WebAssembly module (found by its signature, not its name).
fn wasm_in(a: &BinArgs) -> Result<String, Error> {
    let c = a.op.common();
    let (from, lines) = (c.from, c.lines.max(MIN_LINES));
    let mut ask = nexo_bin::WasmAsk { from, budget: lines, ..Default::default() };
    match &a.op {
        BinOp::Info { .. } => ask.op = "info",
        BinOp::Overview { .. } => ask.op = "overview",
        BinOp::Funcs { sort, grep, min_size, .. } => {
            (ask.op, ask.sort, ask.grep, ask.min_size) = ("funcs", sort.as_str(), grep.as_deref(), *min_size)
        }
        BinOp::Fn { target, bytes, .. } => (ask.op, ask.target, ask.with_bytes) = ("fn", Some(target.as_str()), *bytes),
        BinOp::Calls { target, .. } => (ask.op, ask.target) = ("calls", Some(target.as_str())),
        BinOp::Xref { target, str_text, insensitive, .. } => {
            (ask.op, ask.target, ask.str_text, ask.insensitive) =
                ("xref", target.as_deref(), str_text.as_deref(), *insensitive)
        }
        BinOp::Strings { grep, .. } => (ask.op, ask.grep) = ("strings", grep.as_deref()),
        _ => {
            return Err(Error::Usage(format!(
                "this op does not read WebAssembly modules yet; use one of: {}",
                nexo_bin::WASM_OPS.join(", ")
            )))
        }
    }
    let out = nexo_bin::wasm_answer(a.op.file(), &ask)?;
    let page = out.page_as(from, lines, c.json);
    if c.json {
        serde_json::to_string_pretty(&page.value).map_err(|e| Error::Bad(e.to_string()))
    } else {
        Ok(page.text())
    }
}

/// Print the answer; the exit code is 0, 1 for a question that cannot be answered, 2 for a file that is not read.
pub fn run(a: &BinArgs) -> i32 {
    match execute(a) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("nexo bin: {e}");
            e.exit_code()
        }
    }
}

/// `nexo bin sigs`: sign the functions of static libraries and write the file `nexo bin` reads.
fn make_sigs(libs: &[PathBuf], all: bool, out: Option<&Path>, data_dir: &Path) -> Result<String, Error> {
    let libs = if libs.is_empty() { nexo_bin::sig_libraries() } else { libs.to_vec() };
    if libs.is_empty() {
        return Err(Error::Bad("no static library found here; give one: nexo bin sigs /path/libc.a".into()));
    }
    let keep = if all { nexo_bin::SigKeep::All } else { nexo_bin::SigKeep::Common };
    let mut set = nexo_bin::SigSet::default();
    let mut lines = Vec::new();
    for l in &libs {
        match nexo_bin::sig_generate(l, keep) {
            Ok(s) => {
                lines.push(format!("  {} signatures from {}", s.len(), l.display()));
                set = set.merged(s);
            }
            Err(e) => lines.push(format!("  skipped {}: {e}", l.display())),
        }
    }
    if set.is_empty() {
        return Err(Error::Bad(format!("no signatures made:\n{}", lines.join("\n"))));
    }
    let path = out.map(Path::to_path_buf).unwrap_or_else(|| data_dir.join("sigs").join("local.sig"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Io(format!("cannot create {}: {e}", dir.display())))?;
    }
    let names: Vec<String> =
        libs.iter().filter_map(|l| l.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
    std::fs::write(&path, set.to_text(&names.join(", ")))
        .map_err(|e| Error::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(format!(
        "wrote {} signatures to {}\n{}\nfuncs, fn, overview and decomp name a matching unnamed function and label it [signature ...]; a match is never proof.",
        set.len(),
        path.display(),
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use nexo_bin::fixtures::{self, Fixture};

    /// Parse `nexo bin ...` the way the binary does and answer it, with names kept in `data`.
    fn ask_in(data: &Path, args: &[&str]) -> Result<String, Error> {
        let mut argv = vec!["nexo", "bin"];
        argv.extend_from_slice(args);
        let parsed = crate::cli::Args::try_parse_from(argv).expect("the arguments parse");
        match parsed.cmd {
            Some(crate::cli::Cmd::Bin(b)) => execute_in(&b, data),
            other => panic!("not a bin command: {other:?}"),
        }
    }

    /// The same with a data dir of its own that nothing else uses.
    fn ask(args: &[&str]) -> Result<String, Error> {
        let data = tempfile::tempdir().unwrap();
        ask_in(data.path(), args)
    }

    fn file(dir: &tempfile::TempDir, name: &str, f: &Fixture) -> String {
        let p = dir.path().join(name);
        std::fs::write(&p, &f.bytes).unwrap();
        p.display().to_string()
    }

    #[test]
    fn sigs_makes_a_file_that_names_library_functions_of_a_stripped_program() {
        let Some(libc) = nexo_bin::sig_libraries().into_iter().find(|p| p.file_name().is_some_and(|n| n == "libc.a"))
        else {
            eprintln!("skipped: no libc.a");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path().join("p.c");
        std::fs::write(&c, "#include <string.h>\n#include <stdio.h>\nint main(int n, char **v) { char b[64]; strcpy(b, v[0]); memset(b, n, 9); printf(\"%zu %s\\n\", strlen(v[n - 1]), b); return b[1]; }\n").unwrap();
        let exe = dir.path().join("p.out");
        let built = std::process::Command::new("cc")
            .args(["-O1", "-static", "-fno-pie", "-no-pie", "-s", "-o"])
            .arg(&exe)
            .arg(&c)
            .output()
            .is_ok_and(|o| o.status.success());
        if !built {
            eprintln!("skipped: cannot link a static program here");
            return;
        }
        let data = dir.path().join("data");
        let exe = exe.display().to_string();
        let made = ask_in(&data, &["sigs", &libc.display().to_string()]).unwrap();
        assert!(made.contains("signatures to") && data.join("sigs/local.sig").is_file(), "{made}");
        let t = ask_in(&data, &["funcs", &exe, "--grep", "strlen|memset", "--lines", "40"]).unwrap();
        assert!(t.contains("[signature exact") && t.contains("not proof"), "{t}");
        // without the file the same functions stay unnamed
        let none = ask(&["funcs", &exe, "--grep", "strlen|memset"]).unwrap();
        assert!(!none.contains("[signature exact"), "{none}");
    }

    #[test]
    fn a_wasm_module_is_found_by_its_signature_and_answers_the_same_ops() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("mod.bin");
        std::fs::write(&p, nexo_bin::wasm_fixture::module()).unwrap();
        let path = p.display().to_string();
        let t = ask(&["info", &path]).unwrap();
        assert!(t.contains("WebAssembly module") && t.contains("3 defined functions"), "{t}");
        let t = ask(&["fn", &path, "greet"]).unwrap();
        assert!(t.contains("string? \"hello wasm\"") && t.contains("env.log"), "{t}");
        let t = ask(&["funcs", &path, "--grep", "add", "--json"]).unwrap();
        assert!(t.contains("\"name\": \"demo::add\""), "{t}");
        let t = ask(&["strings", &path, "--grep", "second"]).unwrap();
        assert!(t.contains("second string") && t.contains("<- func[4]"), "{t}");
        let e = ask(&["decomp", &path, "greet"]).unwrap_err().to_string();
        assert!(e.contains("does not read WebAssembly modules yet; use one of: info, overview"), "{e}");
    }

    #[test]
    fn info_fn_bytes_strings_and_find_answer_through_the_parser() {
        let dir = tempfile::tempdir().unwrap();
        for (name, f) in [("elf", fixtures::elf64(false)), ("pe.exe", fixtures::pe64(false))] {
            let path = file(&dir, name, &f);
            let t = ask(&["info", &path]).unwrap();
            assert!(t.contains("x86-64") && t.contains("3 functions"), "{t}");
            let t = ask(&["fn", &path, &format!("{:#x}", f.b), "--bytes"]).unwrap();
            assert!(t.contains("48 8d 3d") && t.contains(fixtures::STRING), "{t}");
            let t = ask(&["bytes", &path, &format!("{:#x}", f.table), "16", "--as", "u32"]).unwrap();
            assert!(t.contains("0x12345678"), "{t}");
            let t = ask(&["strings", &path, "--grep", "nexo"]).unwrap();
            assert!(t.contains(fixtures::STRING), "{t}");
            let t = ask(&["find", &path, "48", "8d", "3d"]).unwrap();
            assert!(t.contains(&format!("{:#010x}", f.b)), "{t}");
            let t = ask(&["find", &path, "--imm", "0x539"]).unwrap();
            assert!(t.contains("mov eax"), "{t}");
        }
    }

    #[test]
    fn overview_answers_through_the_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "elf", &fixtures::elf64_xref());
        let t = ask(&["overview", &path]).unwrap();
        assert!(t.contains("overview") && t.contains("next") && t.contains("nexo bin decomp"), "{t}");
        let j = ask(&["overview", &path, "--json"]).unwrap();
        assert!(j.contains("\"op\": \"overview\""), "{j}");
    }

    #[test]
    fn xref_funcs_calls_and_strings_answer_through_the_parser() {
        let dir = tempfile::tempdir().unwrap();
        for (name, f) in [("elf", fixtures::elf64_xref()), ("pe.exe", fixtures::pe64_xref())] {
            let path = file(&dir, name, &f);
            let b = format!("{:#x}", f.b);
            let t = ask(&["xref", &path, &b]).unwrap();
            assert!(
                t.contains("; 3 references") && t.contains("calls (2)") && t.contains("pointers in data (1)"),
                "{t}"
            );
            let t = ask(&["xref", &path, &format!("{:#x}", f.global.unwrap())]).unwrap();
            assert!(t.contains("reads (1)") && t.contains("writes (1)"), "{t}");
            let t = ask(&["xref", &path, "--str", "NEXO", "--i"]).unwrap();
            assert!(t.contains("1 strings, 2 references") && t.contains("ignoring case"), "{t}");
            let t = ask(&["xref", &path, "--str", "NEXO"]).unwrap();
            assert!(t.contains("no string contains"), "{t}");
            let t = ask(&["funcs", &path, "--sort", "callers", "--lines", "10"]).unwrap();
            assert!(
                t.lines().nth(1).unwrap().contains(&format!("{b:>10}"))
                    || t.contains(&format!("{:#010x}        16        2", f.b)),
                "{t}"
            );
            let t = ask(&["funcs", &path, "--sort", "size", "--min-size", "20", "--grep", "."]).unwrap();
            assert_eq!(t.lines().count(), 2, "{t}");
            let t = ask(&["calls", &path, &format!("{:#x}", f.a)]).unwrap();
            assert!(t.contains("0 callers (0 call sites), 2 callees"), "{t}");
            let t = ask(&["strings", &path, "--sort", "refs", "--grep", "nexo"]).unwrap();
            assert!(t.contains("     2  \"hello nexo bin\""), "{t}");
            let t = ask(&["fn", &path, &b]).unwrap();
            assert!(t.lines().next().unwrap().ends_with("; refs 2"), "{t}");
            // the options of the new ops reach the answer as JSON too
            let j: serde_json::Value = serde_json::from_str(&ask(&["xref", &path, &b, "--json"]).unwrap()).unwrap();
            assert_eq!(j["op"], "xref");
            assert_eq!(j["items"].as_array().unwrap().len(), 3);
            for bad in [vec!["xref", &path as &str], vec!["funcs", &path, "--sort", "nope"], vec!["xref", &path, "--i"]]
            {
                let mut argv = vec!["nexo", "bin"];
                argv.extend(bad);
                assert!(crate::cli::Args::try_parse_from(argv).is_err());
            }
            let e = ask(&["funcs", &path, "--grep", "("]).unwrap_err();
            assert_eq!(e.exit_code(), 1, "{e}");
        }
    }

    #[test]
    fn name_note_names_keep_what_they_are_given_between_calls() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let f = fixtures::elf64_xref();
        let path = file(&dir, "elf", &f);
        let b = format!("{:#x}", f.b);
        let t = ask_in(data.path(), &["name", &path, &b, "load_banner"]).unwrap();
        assert!(t.contains("load_banner"), "{t}");
        ask_in(data.path(), &["note", &path, "load_banner", "prints", "the", "banner"]).unwrap();
        let t = ask_in(data.path(), &["names", &path]).unwrap();
        assert!(t.contains("1 names, 1 notes") && t.contains("load_banner") && t.contains("prints the banner"), "{t}");
        for args in [["fn", &path, "load_banner"], ["xref", &path, "load_banner"], ["calls", &path, "load_banner"]] {
            let t = ask_in(data.path(), &args).unwrap();
            assert!(t.contains("load_banner") && t.contains("; note: prints the banner"), "{args:?}: {t}");
        }
        let t = ask_in(data.path(), &["funcs", &path]).unwrap();
        assert!(t.contains("load_banner"), "{t}");
        // another data dir knows nothing of it
        let t = ask(&["names", &path]).unwrap();
        assert!(t.contains("0 names, 0 notes"), "{t}");
        // refused names exit 1 and leave the file alone
        let e = ask_in(data.path(), &["name", &path, &b, "has space"]).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{e}");
        let t = ask_in(data.path(), &["name", &path, "--clear", &b]).unwrap();
        assert!(t.contains("cleared"), "{t}");
        let t = ask_in(data.path(), &["note", &path, &b, "--clear"]).unwrap();
        assert!(t.contains("cleared"), "{t}");
        assert!(ask_in(data.path(), &["names", &path]).unwrap().contains("0 names, 0 notes"));
        // a broken names file is told on the last line and the answer is still given
        let sha = Image::open(Path::new(&path)).unwrap().sha256.clone();
        let broken = nexo_bin::names_path(data.path(), &sha);
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(&broken, "{\"names\": ").unwrap();
        let t = ask_in(data.path(), &["fn", &path, &b]).unwrap();
        assert!(t.starts_with("fn ") && t.lines().last().unwrap().starts_with("names file not read: "), "{t}");
        let j: serde_json::Value =
            serde_json::from_str(&ask_in(data.path(), &["fn", &path, &b, "--json"]).unwrap()).unwrap();
        assert!(j["warning"].as_str().unwrap().starts_with("names file not read: "), "{j}");
        let e = ask_in(data.path(), &["name", &path, &b, "fresh_name"]).unwrap_err();
        assert_eq!(e.exit_code(), 1, "{e}");
    }

    #[test]
    fn fn_to_and_the_address_page_answer_through_the_parser() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixtures::elf64_xref();
        let path = file(&dir, "elf", &f);
        let end = format!("{:#x}", f.f.unwrap() + 14);
        let t = ask(&["fn", &path, &format!("{:#x}", f.a), "--to", &end]).unwrap();
        assert_eq!(t.lines().filter(|l| l.starts_with("fn ")).count(), 6, "{t}");
        let t = ask(&["fn", &path, &format!("{:#x}", f.e.unwrap() + 10)]).unwrap();
        assert!(t.lines().any(|l| l.starts_with(&format!("> {:#010x}", f.e.unwrap() + 10))), "{t}");
        // the resume command of a cut range keeps --to
        let t = ask(&["fn", &path, &format!("{:#x}", f.a), "--to", &end, "--lines", "10"]).unwrap();
        let last = t.lines().last().unwrap();
        assert!(last.contains(&format!(" --to {end} ")) && last.ends_with("--lines 10"), "{last}");
        // a range that cannot be shown is a usage problem: exit 2
        for (a, to) in
            [(format!("{:#x}", f.b), format!("{:#x}", f.a)), (format!("{:#x}", f.b), format!("{:#x}", f.string))]
        {
            let e = ask(&["fn", &path, &a, "--to", &to]).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{e}");
        }
    }

    #[test]
    fn help_has_one_line_for_each_op() {
        let mut root = crate::cli::Args::command();
        let bin = root.find_subcommand_mut("bin").expect("bin is a command");
        let ops: Vec<String> = bin.get_subcommands().map(|c| c.get_name().to_string()).collect();
        for want in [
            "info", "fn", "xref", "funcs", "calls", "decomp", "proto", "struct", "var", "name", "note", "names",
            "bytes", "strings", "find",
        ] {
            assert!(ops.iter().any(|o| o == want), "{want} is missing from {ops:?}");
        }
        let help = bin.render_help().to_string();
        for op in &ops {
            if op == "help" {
                continue;
            }
            let rows = help.lines().filter(|l| l.trim_start().starts_with(&format!("{op} ")) || l.trim() == op).count();
            assert_eq!(rows, 1, "{op}: {help}");
        }
    }

    #[test]
    fn lines_and_json_cut_the_same_way() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixtures::elf64(false);
        let path = file(&dir, "elf", &f);
        let t = ask(&["info", &path, "--lines", "10"]).unwrap();
        assert_eq!(t.lines().count(), 10);
        let last = t.lines().last().unwrap();
        assert!(
            last.starts_with("... 4 more lines: nexo bin info ")
                && last.contains("--from 0x")
                && last.ends_with("--lines 10"),
            "{last}"
        );
        let from = last.split("--from ").nth(1).unwrap().split(' ').next().unwrap();
        let rest = ask(&["info", &path, "--from", from, "--lines", "10"]).unwrap();
        assert_eq!(rest.lines().count(), 4);
        let j: serde_json::Value =
            serde_json::from_str(&ask(&["info", &path, "--lines", "10", "--json"]).unwrap()).unwrap();
        assert_eq!(j["op"], "info");
        assert_eq!(j["left"], 4);
        assert_eq!(j["items"].as_array().unwrap().len(), 2);
        // the JSON resume command asks for JSON again; apart from that it is the text one
        let next = j["next"].as_str().unwrap();
        assert!(next.contains(" --json --from "), "{next}");
        assert_eq!(next.replace(" --json", ""), last.split(": ").nth(1).unwrap());
    }

    #[test]
    fn a_question_that_cannot_be_answered_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "elf", &fixtures::elf64(false));
        let e = ask(&["fn", &path, "0x10"]).unwrap_err();
        assert_eq!(e.exit_code(), 1);
        assert!(e.to_string().contains("not inside any section"), "{e}");
        // a file that cannot be opened is a file problem: exit 2 like a file that is not read
        let e = ask(&["info", &dir.path().join("missing").display().to_string()]).unwrap_err();
        assert_eq!(e.exit_code(), 2);
    }
}
