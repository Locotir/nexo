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
            | BinOp::Fn { file, .. }
            | BinOp::Bytes { file, .. }
            | BinOp::Strings { file, .. }
            | BinOp::Xref { file, .. }
            | BinOp::Funcs { file, .. }
            | BinOp::Calls { file, .. }
            | BinOp::Name { file, .. }
            | BinOp::Note { file, .. }
            | BinOp::Names { file, .. }
            | BinOp::Find { file, .. } => file,
        }
    }

    fn common(&self) -> &Common {
        match self {
            BinOp::Info { common, .. }
            | BinOp::Fn { common, .. }
            | BinOp::Bytes { common, .. }
            | BinOp::Strings { common, .. }
            | BinOp::Xref { common, .. }
            | BinOp::Funcs { common, .. }
            | BinOp::Calls { common, .. }
            | BinOp::Name { common, .. }
            | BinOp::Note { common, .. }
            | BinOp::Names { common, .. }
            | BinOp::Find { common, .. } => common,
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
    let mut img = Image::open(a.op.file())?;
    let warning = nexo_bin::attach_names(&mut img, data_dir);
    let c = a.op.common();
    let (from, lines) = (c.from, c.lines.max(MIN_LINES));
    let out: Output = match &a.op {
        BinOp::Info { .. } => nexo_bin::info(&img),
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
        BinOp::Names { .. } => nexo_bin::names_list_window(&img, from, lines),
        BinOp::Bytes { addr, len, ty, .. } => nexo_bin::bytes(&img, *addr, *len, ElemType::parse(ty)?)?,
        BinOp::Strings { grep, sort, .. } => {
            nexo_bin::strings_window(&img, grep.as_deref(), StrSort::parse(sort)?, from, lines)?
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
        for want in ["info", "fn", "xref", "funcs", "calls", "name", "note", "names", "bytes", "strings", "find"] {
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
