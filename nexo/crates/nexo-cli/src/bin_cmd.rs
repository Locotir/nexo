//! `nexo bin`: read an ELF or PE x86-64 file as bytes and answer in bounded pieces.
//! The file is never run or loaded. See the `nexo-bin` crate.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use nexo_bin::{ElemType, Error, Image, Output, DEFAULT_LINES, MIN_LINES};

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
}

#[derive(Subcommand, Debug)]
pub enum BinOp {
    /// Format, entry, sections, counts and what the headers say about hardening.
    Info {
        file: PathBuf,
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
    /// List ASCII and UTF-16 strings with their addresses; --grep keeps the ones that match.
    Strings {
        file: PathBuf,
        /// A regular expression.
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
}

fn parse_addr(s: &str) -> Result<u64, String> {
    nexo_bin::parse_addr(s).map_err(|e| e.to_string())
}

impl BinOp {
    fn file(&self) -> &Path {
        match self {
            BinOp::Info { file, .. }
            | BinOp::Fn { file, .. }
            | BinOp::Bytes { file, .. }
            | BinOp::Strings { file, .. }
            | BinOp::Find { file, .. } => file,
        }
    }

    fn common(&self) -> &Common {
        match self {
            BinOp::Info { common, .. }
            | BinOp::Fn { common, .. }
            | BinOp::Bytes { common, .. }
            | BinOp::Strings { common, .. }
            | BinOp::Find { common, .. } => common,
        }
    }
}

/// Answer one question: the text (or JSON) to print.
pub fn execute(a: &BinArgs) -> Result<String, Error> {
    let img = Image::open(a.op.file())?;
    let out: Output = match &a.op {
        BinOp::Info { .. } => nexo_bin::info(&img),
        BinOp::Fn { target, bytes, .. } => nexo_bin::func(&img, target, *bytes)?,
        BinOp::Bytes { addr, len, ty, .. } => nexo_bin::bytes(&img, *addr, *len, ElemType::parse(ty)?)?,
        BinOp::Strings { grep, .. } => nexo_bin::strings(&img, grep.as_deref())?,
        BinOp::Find { pattern, imm, .. } => match imm {
            Some(n) => nexo_bin::find_imm(&img, nexo_bin::parse_number(n)?)?,
            None => nexo_bin::find_bytes(&img, &pattern.join(" "))?,
        },
    };
    let c = a.op.common();
    let page = out.page(c.from, c.lines.max(MIN_LINES));
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use nexo_bin::fixtures::{self, Fixture};

    /// Parse `nexo bin ...` the way the binary does and answer it.
    fn ask(args: &[&str]) -> Result<String, Error> {
        let mut argv = vec!["nexo", "bin"];
        argv.extend_from_slice(args);
        let parsed = crate::cli::Args::try_parse_from(argv).expect("the arguments parse");
        match parsed.cmd {
            Some(crate::cli::Cmd::Bin(b)) => execute(&b),
            other => panic!("not a bin command: {other:?}"),
        }
    }

    fn file(dir: &tempfile::TempDir, name: &str, f: &Fixture) -> String {
        let p = dir.path().join(name);
        std::fs::write(&p, &f.bytes).unwrap();
        p.display().to_string()
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
        assert_eq!(j["next"].as_str().unwrap(), last.split(": ").nth(1).unwrap());
    }

    #[test]
    fn a_question_that_cannot_be_answered_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "elf", &fixtures::elf64(false));
        let e = ask(&["fn", &path, "0x10"]).unwrap_err();
        assert_eq!(e.exit_code(), 1);
        assert!(e.to_string().contains("not inside any section"), "{e}");
        let e = ask(&["info", &dir.path().join("missing").display().to_string()]).unwrap_err();
        assert_eq!(e.exit_code(), 1);
    }
}
