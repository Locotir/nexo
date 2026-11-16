use clap::Args;
use nexo_bin::{Lines, PageAsk};

use super::{parse_addr, parse_lines, BinOp};

#[derive(Args, Debug)]
#[command(arg_required_else_help = true, args_conflicts_with_subcommands = true)]
pub struct BinArgs {
    #[command(subcommand)]
    pub op: Option<BinOp>,
    /// Serve binary tools over MCP on stdio.
    #[arg(long)]
    pub mcp: bool,
    /// With --mcp: allow tools that write files or saved state.
    #[arg(long, requires = "mcp")]
    pub allow_changes: bool,
}

/// Options every op takes.
#[derive(Args, Debug)]
pub struct Common {
    /// Lines in the answer: a number (at least 10) or all. Default 120, or all of a range or length that was asked
    /// for (A..B, fn --to, bytes). When it is cut, the last line gives the command for the next piece.
    #[arg(long, value_name = "N|all", value_parser = parse_lines)]
    pub lines: Option<Lines>,
    /// Keep only the rows that match this regular expression, then page them (ops that filter by name or text
    /// themselves, like funcs and strings, use it there). The first line says how many rows matched.
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,
    /// Start at this address (or line position); the cut line prints the value to use.
    #[arg(long, value_parser = parse_addr, value_name = "ADDR")]
    pub from: Option<u64>,
    /// Print the answer as JSON (same bound).
    #[arg(long)]
    pub json: bool,
    /// Print on stderr how long the references took to build and how long the whole answer took.
    #[arg(long)]
    pub timing: bool,
    /// The slice of a universal (fat) Mach-O file to read: x86-64 or arm64 (default: the first of those). With
    /// --raw, the machine the bytes are code for: x86-64 (the default), x86-32 or arm64.
    #[arg(long, value_name = "ARCH")]
    pub arch: Option<String>,
    /// The file is bare code with no headers (a memory dump, shellcode, JIT output, an unpacked stub): one
    /// readable, writable and executable section loaded at --base, its first byte the entry.
    #[arg(long)]
    pub raw: bool,
    /// With --raw: the address the first byte was loaded at (default 0).
    #[arg(long, value_parser = parse_addr, value_name = "ADDR", requires = "raw")]
    pub base: Option<u64>,
}

impl Common {
    /// The page this call asks for; `sized` when it named a range or a length.
    pub fn ask(&self, sized: bool) -> PageAsk {
        PageAsk { lines: self.lines, from: self.from, grep: self.grep.clone(), json: self.json, sized }
    }
}
