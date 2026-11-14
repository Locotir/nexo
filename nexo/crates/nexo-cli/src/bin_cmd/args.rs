use clap::Args;
use nexo_bin::{DEFAULT_LINES, MIN_LINES};

use super::{parse_addr, BinOp};

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
    /// Most lines in the answer (at least 10; default 120, or the range asked for by fn --to and bytes, up to 400).
    /// When it is cut, the last line gives the command for the next piece.
    #[arg(long, value_name = "N")]
    pub lines: Option<usize>,
    /// Start at this address (or line position); the cut line prints the value to use.
    #[arg(long, value_parser = parse_addr, value_name = "ADDR")]
    pub from: Option<u64>,
    /// Print the answer as JSON (same bound).
    #[arg(long)]
    pub json: bool,
    /// Print on stderr how long the references took to build and how long the whole answer took.
    #[arg(long)]
    pub timing: bool,
    /// The slice of a universal (fat) Mach-O file to read: x86-64 or arm64 (default: the first of those).
    #[arg(long, value_name = "ARCH")]
    pub arch: Option<String>,
}

impl Common {
    /// The lines of a page: `--lines`, or the default when it was not given.
    pub fn budget(&self) -> usize {
        self.lines.unwrap_or(DEFAULT_LINES).max(MIN_LINES)
    }
}
