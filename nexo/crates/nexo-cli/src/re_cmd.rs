//! `nexo re`: the one entry point of nexo RE, the reverse-engineering kit. With no word it prints a map of
//! `nexo bin` (read a file) and `nexo dbg` (run it); with a word it runs that op or command. The map is
//! built from the clap definition of `nexo bin` and the command table of `nexo-dbg`, so it cannot drift.

use clap::{Args, CommandFactory, Parser};

use crate::dbg_cmd::DbgArgs;

#[derive(Args, Debug)]
pub struct ReArgs {
    /// Nothing prints the map. A `nexo bin` op or a `nexo dbg` command runs it, with the rest of the words.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., value_name = "WORD")]
    pub words: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Route {
    Bin,
    Dbg,
}

/// Every op of `nexo bin`: `(name, aliases, one line)`, read from its clap definition.
fn bin_ops() -> Vec<(String, Vec<String>, String)> {
    let root = crate::cli::Args::command();
    let bin = root.find_subcommand("bin").expect("bin is a command");
    bin.get_subcommands()
        .map(|c| {
            let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
            (c.get_name().to_string(), c.get_all_aliases().map(str::to_string).collect(), first_sentence(&about))
        })
        .collect()
}

/// The text up to its first full stop, at most 100 characters.
fn first_sentence(s: &str) -> String {
    let s = s.lines().next().unwrap_or("").trim();
    let s = s.split(". ").next().unwrap_or(s).trim_end_matches('.');
    if s.chars().count() <= 100 {
        return s.to_string();
    }
    let cut: String = s.chars().take(97).collect();
    format!("{}...", cut.trim_end())
}

fn is_bin_op(word: &str, ops: &[(String, Vec<String>, String)]) -> bool {
    ops.iter().any(|(n, a, _)| n == word || a.iter().any(|x| x == word))
}

/// Which command a word names. A word both take goes to `nexo bin`.
fn route(word: &str) -> Option<Route> {
    if is_bin_op(word, &bin_ops()) {
        Some(Route::Bin)
    } else if nexo_dbg::canonical(word).is_some() {
        Some(Route::Dbg)
    } else {
        None
    }
}

/// The words both `nexo bin` and `nexo dbg` take.
fn clashes(ops: &[(String, Vec<String>, String)]) -> Vec<&'static str> {
    nexo_dbg::COMMANDS.iter().flat_map(|(n, _)| n.split(' ')).filter(|w| is_bin_op(w, ops)).collect()
}

/// The map: one line for each `nexo bin` op and each `nexo dbg` command.
pub fn map() -> String {
    let ops = bin_ops();
    let width = ops.iter().map(|o| o.0.len()).chain(nexo_dbg::COMMANDS.iter().map(|c| first_name(c.0).len())).max();
    let width = width.unwrap_or(8);
    let mut out = vec![
        "nexo RE: read a file with `nexo bin <op> <file>`, run it with `nexo dbg <command>`; `nexo re <word> ...` picks the right one.".to_string(),
        "read a file (nexo bin; the file is never run):".to_string(),
    ];
    out.extend(ops.iter().map(|(n, _, about)| format!("  {n:<width$}  {about}")));
    out.push("run it (nexo dbg; a live program, one bounded call at a time):".to_string());
    out.extend(nexo_dbg::COMMANDS.iter().map(|(n, about)| format!("  {:<width$}  {about}", first_name(n))));
    let both = clashes(&ops);
    if !both.is_empty() {
        out.push(format!(
            "in both: {}. `nexo re` takes the `nexo bin` one; write `nexo dbg <word>` for the debugger's.",
            both.join(", ")
        ));
    }
    out.push("details: nexo bin <op> --help | nexo dbg help".to_string());
    out.join("\n")
}

fn first_name(names: &str) -> &str {
    names.split(' ').next().unwrap_or(names)
}

/// The error for a word that is neither: the nearest names.
fn unknown(word: &str) -> String {
    let w = word.to_lowercase();
    let mut names: Vec<String> = bin_ops().into_iter().flat_map(|(n, a, _)| std::iter::once(n).chain(a)).collect();
    names.extend(nexo_dbg::COMMANDS.iter().flat_map(|(n, _)| n.split(' ')).map(str::to_string));
    names.sort();
    names.dedup();
    names.sort_by_key(|n| (!n.starts_with(&w) && !w.starts_with(n.as_str()), crate::first_use::distance(&w, n)));
    names.truncate(3);
    format!(
        "{word:?} is neither a `nexo bin` op nor a `nexo dbg` command; nearest: {}. `nexo re` lists them all.",
        names.join(", ")
    )
}

/// What to do with the words: print the map, or hand them to `nexo bin` or `nexo dbg`.
enum Plan {
    Map,
    Bin(Vec<String>),
    Dbg(Vec<String>),
    Unknown(String),
}

fn plan(words: &[String]) -> Plan {
    let Some(first) = words.first() else { return Plan::Map };
    if matches!(first.as_str(), "help" | "-h" | "--help") {
        return Plan::Map;
    }
    match route(first) {
        Some(Route::Bin) => Plan::Bin(words.to_vec()),
        Some(Route::Dbg) => Plan::Dbg(words.to_vec()),
        None => Plan::Unknown(unknown(first)),
    }
}

pub fn run(a: &ReArgs) -> i32 {
    match plan(&a.words) {
        Plan::Map => {
            println!("{}", map());
            0
        }
        Plan::Unknown(why) => {
            eprintln!("nexo re: {why}");
            crate::headless::EXIT_USAGE
        }
        Plan::Dbg(words) => crate::dbg_cmd::run(&DbgArgs { words }),
        Plan::Bin(words) => {
            let argv = ["nexo".to_string(), "bin".to_string()].into_iter().chain(words);
            match crate::cli::Args::try_parse_from(argv) {
                Ok(parsed) => match parsed.cmd {
                    Some(crate::cli::Cmd::Bin(b)) => crate::bin_cmd::run(&b),
                    _ => crate::headless::EXIT_USAGE,
                },
                Err(e) => e.exit(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn the_map_lists_every_bin_op() {
        let m = map();
        let ops = bin_ops();
        assert!(ops.len() >= 20, "{ops:?}");
        for (n, _, _) in &ops {
            assert!(m.lines().any(|l| l.trim_start().starts_with(&format!("{n} "))), "{n} is missing:\n{m}");
        }
    }

    #[test]
    fn the_map_lists_every_dbg_command() {
        let m = map();
        for (names, _) in nexo_dbg::COMMANDS {
            let n = first_name(names);
            assert!(m.lines().any(|l| l.trim_start().starts_with(&format!("{n} "))), "{n} is missing:\n{m}");
        }
    }

    #[test]
    fn the_map_has_a_title_a_group_for_each_and_a_last_line_for_details() {
        let m = map();
        let lines: Vec<&str> = m.lines().collect();
        assert!(lines[0].starts_with("nexo RE:"));
        assert!(m.contains("read a file (nexo bin"));
        assert!(m.contains("run it (nexo dbg"));
        assert_eq!(lines.last().copied(), Some("details: nexo bin <op> --help | nexo dbg help"));
        assert!(lines.iter().all(|l| l.chars().count() < 160), "a line is too long");
    }

    #[test]
    fn the_map_says_which_command_wins_for_a_word_both_take() {
        let both = clashes(&bin_ops());
        assert!(both.contains(&"info"), "{both:?}");
        assert!(map().contains("in both: "));
        assert_eq!(route("info"), Some(Route::Bin));
        assert_eq!(route("find"), Some(Route::Bin));
    }

    #[test]
    fn words_go_to_nexo_bin() {
        assert_eq!(route("decomp"), Some(Route::Bin));
        assert!(matches!(plan(&w("decomp ./a.out main")), Plan::Bin(x) if x == w("decomp ./a.out main")));
        assert_eq!(route("strings"), Some(Route::Bin));
    }

    #[test]
    fn words_go_to_nexo_dbg() {
        assert_eq!(route("run"), Some(Route::Dbg));
        assert!(matches!(plan(&w("run ./a.out")), Plan::Dbg(x) if x == w("run ./a.out")));
        assert_eq!(route("bt"), Some(Route::Dbg));
        assert_eq!(route("c"), Some(Route::Dbg));
    }

    #[test]
    fn no_word_and_help_print_the_map() {
        assert!(matches!(plan(&[]), Plan::Map));
        assert!(matches!(plan(&w("--help")), Plan::Map));
    }

    #[test]
    fn an_unknown_word_lists_the_nearest_names() {
        let Plan::Unknown(why) = plan(&w("decmop ./a.out")) else { panic!("should be unknown") };
        assert!(why.contains("\"decmop\""), "{why}");
        let near = why.split("nearest: ").nth(1).expect("names follow");
        assert!(near.starts_with("decomp"), "{why}");
        let Plan::Unknown(why) = plan(&w("brk")) else { panic!("should be unknown") };
        assert!(why.contains("break"), "{why}");
    }

    #[test]
    fn a_word_that_starts_with_a_dash_is_not_a_command() {
        assert!(matches!(plan(&w("--lines 5")), Plan::Unknown(_)));
    }
}
