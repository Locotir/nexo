use super::clap_facts::not_an_op;
use super::*;

/// One op line, understood: the answer of `--help`, or the op to run.
pub(super) enum Parsed {
    Help(String),
    Op(Box<crate::bin_cmd::BinArgs>),
}

pub(super) fn parse(unit: &str) -> Result<Parsed, String> {
    let words = crate::re_mcp::split_words(unit)?;
    let mut words = words.as_slice();
    // a model that writes the shell form (`nexo bin fn ./p main`) gets the same answer
    if words.first().is_some_and(|w| w == "nexo") {
        words = &words[1..];
    }
    if words.first().is_some_and(|w| w == "bin") {
        words = &words[1..];
    }
    let mut argv: Vec<String> = vec!["nexo".into(), "bin".into()];
    match words.first().map(String::as_str) {
        None | Some("help") => argv.push("--help".into()),
        Some(w) => {
            if let Some(fact) = not_an_op(w) {
                return Err(fact);
            }
            argv.extend(words.iter().cloned())
        }
    }
    let parsed = match crate::cli::Args::try_parse_from(argv) {
        Ok(p) => p,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            return Ok(Parsed::Help(e.to_string()))
        }
        Err(e) => {
            return Err(bin_error(&e, words).unwrap_or_else(|| {
                let text = e.to_string();
                text.strip_prefix("error: ").unwrap_or(&text).to_string()
            }))
        }
    };
    let Some(crate::cli::Cmd::Bin(b)) = parsed.cmd else {
        return Err("bin: not a bin op".into());
    };
    if b.op.is_none() {
        return Err("bin: give an op, for example `overview ./prog`; `--help` lists them".into());
    }
    Ok(Parsed::Op(b))
}
