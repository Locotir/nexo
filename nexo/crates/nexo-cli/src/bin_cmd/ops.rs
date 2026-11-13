use std::path::PathBuf;

use clap::Subcommand;
use nexo_bin::{ElemType, FuncSort, StrSort};

use super::{parse_addr, Common};

#[derive(Subcommand, Debug)]
pub enum BinOp {
    /// Assemble Intel x86 instructions (semicolon/newline separated); labels and rel32 branches supported.
    Asm {
        source: String,
        #[arg(long, default_value = "0", value_parser = parse_addr)]
        at: u64,
        #[arg(long, default_value = "64", value_parser = ["64", "32"])]
        bits: String,
        #[command(flatten)]
        common: Common,
    },
    /// Patch generic bytes into a new copy; never modifies the input or overwrites an existing output.
    #[command(group(clap::ArgGroup::new("location").required(true).args(["at", "function"])),
              group(clap::ArgGroup::new("replacement").required(true).args(["bytes", "asm", "nop"])))]
    Patch {
        file: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, value_parser = parse_addr)]
        at: Option<u64>,
        #[arg(long = "fn", value_name = "NAME+OFF")]
        function: Option<String>,
        #[arg(long, value_name = "HEX")]
        bytes: Option<String>,
        #[arg(long, value_name = "INTEL")]
        asm: Option<String>,
        #[arg(long, value_name = "N")]
        nop: Option<usize>,
        #[command(flatten)]
        common: Common,
    },
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
        /// Go forward instead: where the value goes (compares, calls it is passed to, stores, copies).
        #[arg(long)]
        forward: bool,
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
    /// JVM .class and .jar files and .NET assemblies (.exe, .dll): types, members, bytecode with resolved names, strings, call sites.
    Managed {
        file: PathBuf,
        /// A type (lists its members) or a method like Type.name or Type.name(II)I (disassembles it).
        target: Option<String>,
        /// List the strings: constant-pool ones and the ones code loads.
        #[arg(long)]
        strings: bool,
        /// List every call site of a method: Owner.name (descriptor optional).
        #[arg(long, value_name = "OWNER.NAME")]
        callers: Option<String>,
        /// A regular expression that keeps matching types, members or strings.
        #[arg(long, value_name = "REGEX")]
        grep: Option<String>,
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
    /// Compare two builds function by function: the changed, added and removed ones, with the size change.
    Diff {
        /// The old build.
        file: PathBuf,
        /// The new build.
        other: PathBuf,
        /// One function (a name, an address in the old build, or b:ADDR in the new one): its instructions side by side.
        target: Option<String>,
        /// A regular expression that keeps the functions whose name matches.
        #[arg(long, value_name = "REGEX")]
        grep: Option<String>,
        /// List the same functions too (with a function: every instruction, none folded).
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        common: Common,
    },
}
