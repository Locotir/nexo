//! Several `nexo bin` ops in one go, for the three surfaces that take them: the agent tool (`a; b`), the MCP tool
//! `bin_batch` and `nexo bin --script FILE`. One splitter (`units`), one way to run a unit (`run_argv`).

use std::io::Read;

use clap::error::ErrorKind;
use clap::Parser;

use super::bridge::{self, Reader};

/// The ops of one call: cut at `;` and at line ends outside quotes, `#` lines left out, as `debug` cuts a script.
/// A part with a quote still open (a note that says `a; b` inside single quotes) is joined to the next, so text
/// stays whole.
pub(crate) fn units(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for u in nexo_dbg::script_units(line) {
        match out.last_mut() {
            Some(last) if open => {
                last.push_str("; ");
                last.push_str(&u);
            }
            _ => out.push(u),
        }
        open = out.last().is_some_and(|l| crate::re_mcp::split_words(l).is_err());
    }
    out
}

/// The words of one unit after `nexo bin` (a model that writes the shell form, `nexo bin fn ./p main`, is read the
/// same).
pub(crate) fn unit_words(unit: &str) -> Result<Vec<String>, String> {
    let mut words = crate::re_mcp::split_words(unit)?;
    for lead in ["nexo", "bin"] {
        if words.first().is_some_and(|w| w == lead) {
            words.remove(0);
        }
    }
    Ok(words)
}

/// The full argv (`nexo bin ...`) of one unit.
pub(crate) fn unit_argv(unit: &str) -> Result<Vec<String>, String> {
    let mut argv = vec!["nexo".to_string(), "bin".to_string()];
    argv.extend(unit_words(unit)?);
    Ok(argv)
}

/// Run one `nexo bin ...` argv: the answer (with the live-debugging hint for `reader`), or the message and the
/// exit code `nexo bin` gives for it (2 for usage, 1 for a question that cannot be answered).
pub(crate) fn run_argv(argv: Vec<String>, reader: Reader) -> Result<String, (String, i32)> {
    let parsed = match crate::cli::Args::try_parse_from(argv) {
        Ok(p) => p,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => return Ok(e.to_string()),
        Err(e) => return Err((e.to_string(), 2)),
    };
    let Some(crate::cli::Cmd::Bin(b)) = parsed.cmd else { return Err(("nexo bin: not a bin op".to_string(), 2)) };
    let Some(op) = b.op.as_ref() else {
        return Err(("nexo bin: give an op, for example `overview ./prog`".to_string(), 2));
    };
    match super::execute(&b) {
        Ok(text) => Ok(match (bridge::line_for(op, &text, reader), reader) {
            // on the command line the hint goes to stderr, so stdout stays the answer alone
            (Some(l), Reader::Cli) => {
                eprintln!("{l}");
                text
            }
            (Some(l), _) => format!("{text}\n{l}"),
            (None, _) => text,
        }),
        Err(e) => Err((format!("nexo bin: {e}"), e.exit_code())),
    }
}

/// `nexo bin --script FILE` (`-`: stdin): each unit in order under a `== unit ==` line; the first error stops the
/// rest unless `keep_going`. The exit code is the worst one of the ops that failed.
pub fn run_script(path: &str, keep_going: bool) -> i32 {
    let mut text = String::new();
    let read = if path == "-" {
        std::io::stdin().read_to_string(&mut text).map(|_| ())
    } else {
        std::fs::read_to_string(path).map(|t| text = t)
    };
    if let Err(e) = read {
        eprintln!("nexo bin: cannot read the script {path}: {e}");
        return 2;
    }
    let mut code = 0;
    for unit in units(&text) {
        println!("== {unit} ==");
        let one = unit_argv(&unit).map_err(|e| (e, 2)).and_then(|argv| run_argv(argv, Reader::Cli));
        match one {
            Ok(out) => println!("{out}"),
            Err((msg, c)) => {
                eprintln!("{}", msg.strip_prefix("error: ").unwrap_or(&msg).trim_end());
                code = code.max(c);
                if !keep_going {
                    break;
                }
            }
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_cut_at_semicolons_and_lines() {
        assert_eq!(units("info a; info b"), ["info a", "info b"]);
        assert_eq!(units("info a\ninfo b\n"), ["info a", "info b"]);
        assert_eq!(units("info a;\n;; info b"), ["info a", "info b"]);
    }

    #[test]
    fn a_quote_keeps_its_semicolons_and_newlines() {
        assert_eq!(units("note p 0x1 'a; b'; info p"), ["note p 0x1 'a; b'", "info p"]);
        assert_eq!(units("note p 0x1 \"a; b\"; info p"), ["note p 0x1 \"a; b\"", "info p"]);
        assert_eq!(units("note p 0x1 \"a\nb\"\ninfo p"), ["note p 0x1 \"a\nb\"", "info p"]);
    }

    #[test]
    fn comments_and_empty_units_are_left_out() {
        assert!(units("# only a comment").is_empty());
        assert!(units("").is_empty());
        assert!(units(" ;  ; \n\n").is_empty());
        assert_eq!(units("# first\ninfo a\n# last"), ["info a"]);
    }

    #[test]
    fn a_unit_reads_the_shell_form_too() {
        assert_eq!(unit_words("nexo bin fn ./p main").unwrap(), ["fn", "./p", "main"]);
        assert_eq!(unit_words("bin info './my prog'").unwrap(), ["info", "./my prog"]);
        assert!(unit_words("info 'open").is_err());
        assert_eq!(unit_argv("info ./p").unwrap(), ["nexo", "bin", "info", "./p"]);
    }

    #[test]
    fn script_flags_parse_and_conflict_with_an_op() {
        let ok = |a: &[&str]| {
            let mut argv = vec!["nexo", "bin"];
            argv.extend_from_slice(a);
            crate::cli::Args::try_parse_from(argv)
        };
        let Some(crate::cli::Cmd::Bin(b)) = ok(&["--script", "ops.txt", "--keep-going"]).unwrap().cmd else {
            panic!("bin")
        };
        assert!(b.op.is_none() && b.script.as_deref() == Some("ops.txt") && b.keep_going);
        let Some(crate::cli::Cmd::Bin(b)) = ok(&["--script", "-"]).unwrap().cmd else { panic!("bin") };
        assert!(b.script.as_deref() == Some("-") && !b.keep_going);
        assert!(ok(&["--keep-going"]).is_err());
        assert!(ok(&["--script", "x", "info", "./p"]).is_err());
    }

    #[test]
    fn a_script_gives_the_exit_code_of_the_op_that_failed() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ops.txt");
        std::fs::write(&f, "# nothing to read here\ninfo /nonexistent/none\ninfo /nonexistent/other\n").unwrap();
        let p = f.to_string_lossy().to_string();
        assert_ne!(run_script(&p, false), 0);
        assert_ne!(run_script(&p, true), 0);
        assert_eq!(run_script("/nonexistent/ops.txt", false), 2);
    }
}
