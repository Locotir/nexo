//! What each `nexo bin` op does to saved state, said once. The MCP gate (`--allow-changes`), the tool texts, the
//! agent tool's write gate and the `--module` check all ask [`BinOp::effect`], so they cannot drift apart. A new op
//! does not compile until it is classified here.

use clap::Parser;

use super::BinOp;

/// What an op leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Nothing is saved.
    ReadOnly,
    /// Names, notes, types, signatures or a decompiler setting go to nexo's own data folder for this binary.
    SavesNotes,
    /// A file the person named is written (a patched copy, a signature file).
    WritesFile,
}

impl Effect {
    /// Saves anything at all: what the MCP server refuses without `--allow-changes`.
    pub(crate) fn saves(self) -> bool {
        self != Effect::ReadOnly
    }

    /// Writes a file the person named: a write for the agent tool.
    pub(crate) fn writes_file(self) -> bool {
        self == Effect::WritesFile
    }
}

impl BinOp {
    /// What this op line does to saved state. Every op is named, so a new one must be classified here.
    pub(crate) fn effect(&self) -> Effect {
        match self {
            BinOp::Patch { .. } => Effect::WritesFile,
            BinOp::Sigs { out: Some(_), .. } => Effect::WritesFile,
            BinOp::Sigs { out: None, .. } => Effect::SavesNotes,
            BinOp::Crypto { save: true, .. } => Effect::SavesNotes,
            BinOp::Crypto { save: false, .. } => Effect::ReadOnly,
            BinOp::Name { .. }
            | BinOp::Note { .. }
            | BinOp::Proto { .. }
            | BinOp::Struct { .. }
            | BinOp::Var { .. } => Effect::SavesNotes,
            BinOp::Asm { .. }
            | BinOp::Info { .. }
            | BinOp::Syms { .. }
            | BinOp::Overview { .. }
            | BinOp::Slice { .. }
            | BinOp::Path { .. }
            | BinOp::Tree { .. }
            | BinOp::Fn { .. }
            | BinOp::Decomp { .. }
            | BinOp::Xref { .. }
            | BinOp::Funcs { .. }
            | BinOp::Calls { .. }
            | BinOp::Cfg { .. }
            | BinOp::Names { .. }
            | BinOp::Emu { .. }
            | BinOp::Bytes { .. }
            | BinOp::Strings { .. }
            | BinOp::Managed { .. }
            | BinOp::Find { .. }
            | BinOp::Diff { .. } => Effect::ReadOnly,
        }
    }

    /// Why `--module` cannot be used with this op because it reads no module of a running program, or None. A
    /// separate axis from [`BinOp::effect`]: none of these reasons is about saving.
    pub(crate) fn no_module_reason(&self) -> Option<&'static str> {
        match self {
            BinOp::Diff { .. } => Some("it compares two files"),
            BinOp::Asm { .. } | BinOp::Sigs { .. } => Some("it reads no module"),
            BinOp::Managed { .. } => Some("it reads a jar or a .NET assembly"),
            BinOp::Patch { .. }
            | BinOp::Info { .. }
            | BinOp::Syms { .. }
            | BinOp::Overview { .. }
            | BinOp::Crypto { .. }
            | BinOp::Slice { .. }
            | BinOp::Path { .. }
            | BinOp::Tree { .. }
            | BinOp::Fn { .. }
            | BinOp::Decomp { .. }
            | BinOp::Proto { .. }
            | BinOp::Struct { .. }
            | BinOp::Var { .. }
            | BinOp::Xref { .. }
            | BinOp::Funcs { .. }
            | BinOp::Calls { .. }
            | BinOp::Cfg { .. }
            | BinOp::Name { .. }
            | BinOp::Note { .. }
            | BinOp::Names { .. }
            | BinOp::Emu { .. }
            | BinOp::Bytes { .. }
            | BinOp::Strings { .. }
            | BinOp::Find { .. } => None,
        }
    }
}

/// One valid argument tail (after `nexo bin`) for every op, and a second one for the ops whose effect depends on a
/// flag. The first sample of a name has no flag that saves. The tool texts ask these for the effect of an op by
/// name; a test checks that every op of the clap definition has one.
pub(crate) const SAMPLES: &[&[&str]] = &[
    &["asm", "nop"],
    &["patch", "f", "--out", "o", "--at", "0", "--nop", "1"],
    &["info", "f"],
    &["syms", "f"],
    &["overview", "f"],
    &["crypto", "f"],
    &["crypto", "f", "--save"],
    &["slice", "f", "0"],
    &["path", "f", "a", "b"],
    &["tree", "f", "main"],
    &["fn", "f", "main"],
    &["decomp", "f", "main"],
    &["proto", "f", "main", "int f()"],
    &["struct", "f", "s", "0:i32:id"],
    &["var", "f", "main", "a1", "x"],
    &["xref", "f", "main"],
    &["sigs"],
    &["sigs", "--out", "o"],
    &["funcs", "f"],
    &["calls", "f", "main"],
    &["cfg", "f", "main"],
    &["name", "f", "main", "x"],
    &["note", "f", "main", "hi"],
    &["names", "f"],
    &["emu", "f", "main"],
    &["bytes", "f", "0"],
    &["strings", "f"],
    &["managed", "f"],
    &["find", "f", "90"],
    &["diff", "f", "g"],
];

/// The op a words list (after `nexo bin`) parses to, or None when it does not parse as an op.
fn parse_op(tail: &[&str]) -> Option<BinOp> {
    let mut argv = vec!["nexo", "bin"];
    argv.extend_from_slice(tail);
    match crate::cli::Args::try_parse_from(argv).ok()?.cmd {
        Some(crate::cli::Cmd::Bin(b)) => b.op,
        _ => None,
    }
}

/// The effect of the op called `name` with no saving flag, from its first sample; None for a name nexo bin lacks.
pub(crate) fn effect_by_name(name: &str) -> Option<Effect> {
    let tail = SAMPLES.iter().find(|s| s.first() == Some(&name))?;
    parse_op(tail).map(|o| o.effect())
}

/// The names of the ops that save with at least one sample, in order, and those of them that also have a sample
/// that does not save (they save only with a flag).
pub(crate) fn saving_names() -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut all, mut flagged) = (Vec::new(), Vec::new());
    for tail in SAMPLES {
        let Some(name) = tail.first().copied() else { continue };
        if !parse_op(tail).is_some_and(|o| o.effect().saves()) {
            continue;
        }
        if !all.contains(&name) {
            all.push(name);
        }
        if effect_by_name(name) == Some(Effect::ReadOnly) && !flagged.contains(&name) {
            flagged.push(name);
        }
    }
    (all, flagged)
}

/// The effect of one `nexo bin` argv (`nexo bin OP ...`): what its parsed op says. A line that does not parse
/// cannot run, but it is judged as strictly as the old name list did: by its op name, and a `--save` word saves.
pub(crate) fn argv_effect(argv: &[String]) -> Effect {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match crate::cli::Args::try_parse_from(words.iter().copied()) {
        Ok(a) => match a.cmd {
            Some(crate::cli::Cmd::Bin(b)) => b.op.as_ref().map_or(Effect::ReadOnly, BinOp::effect),
            _ => Effect::ReadOnly,
        },
        Err(_) => {
            let by_name = words.get(2).and_then(|n| effect_by_name(n)).unwrap_or(Effect::ReadOnly);
            if by_name == Effect::ReadOnly && words.contains(&"--save") {
                Effect::SavesNotes
            } else {
                by_name
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::super::live;
    use super::*;

    fn op(tail: &[&str]) -> BinOp {
        parse_op(tail).unwrap_or_else(|| panic!("sample {tail:?} does not parse"))
    }

    fn argv_of(tail: &[&str]) -> Vec<String> {
        ["nexo", "bin"].iter().chain(tail.iter()).map(|w| w.to_string()).collect()
    }

    #[test]
    fn every_op_of_the_clap_definition_has_a_sample() {
        let mut root = crate::cli::Args::command();
        let bin = root.find_subcommand_mut("bin").expect("bin is a command");
        for name in bin.get_subcommands().map(|c| c.get_name().to_string()).filter(|n| n != "help") {
            assert!(SAMPLES.iter().any(|s| s.first() == Some(&name.as_str())), "{name} has no sample in SAMPLES");
        }
        for tail in SAMPLES {
            op(tail);
        }
    }

    // The three gates as they were before `BinOp::effect`, kept here as the table the new one must equal.
    fn old_mcp_saves(tail: &[&str]) -> bool {
        ["name", "note", "proto", "struct", "var", "sigs", "patch"].contains(&tail[0])
            || (tail[0] == "crypto" && tail.contains(&"--save"))
    }

    fn old_tool_writes(tail: &[&str]) -> bool {
        tail[0] == "patch" || (tail[0] == "sigs" && tail.contains(&"--out"))
    }

    fn old_live_refuses(tail: &[&str]) -> bool {
        ["patch", "name", "note", "proto", "struct", "var", "diff", "asm", "sigs", "managed"].contains(&tail[0])
            || (tail[0] == "crypto" && tail.contains(&"--save"))
    }

    #[test]
    fn the_gates_agree_with_the_old_lists_for_every_op() {
        for tail in SAMPLES {
            let o = op(tail);
            let e = o.effect();
            assert_eq!(e.saves(), old_mcp_saves(tail), "MCP gate for {tail:?}");
            assert_eq!(argv_effect(&argv_of(tail)).saves(), old_mcp_saves(tail), "argv gate for {tail:?}");
            assert_eq!(e.writes_file(), old_tool_writes(tail), "agent write gate for {tail:?}");
            assert_eq!(live::check(&o).is_err(), old_live_refuses(tail), "--module check for {tail:?}");
            assert!(!e.writes_file() || e.saves(), "a file write is a save: {tail:?}");
            assert!(
                o.no_module_reason().is_none() || live::check(&o).is_err(),
                "no-module reason, but allowed: {tail:?}"
            );
            // --raw is refused for every op
            let mut raw = tail.to_vec();
            raw.insert(1, "--raw");
            assert!(live::check(&op(&raw)).is_err(), "--raw with {tail:?}");
        }
    }

    #[test]
    fn a_line_that_does_not_parse_is_judged_as_strictly_as_before() {
        assert!(argv_effect(&argv_of(&["name", "--bogus"])).saves());
        assert!(argv_effect(&argv_of(&["patch", "--bogus"])).saves());
        assert!(argv_effect(&argv_of(&["crypto", "f", "--save", "--bogus"])).saves());
        assert!(!argv_effect(&argv_of(&["info", "--bogus"])).saves());
        assert!(!argv_effect(&argv_of(&["nosuchop"])).saves());
    }

    #[test]
    fn the_tool_texts_name_the_same_ops() {
        let (all, flagged) = saving_names();
        for n in ["patch", "name", "note", "proto", "struct", "var", "sigs", "crypto"] {
            assert!(all.contains(&n), "{n} is missing from the saving names");
        }
        assert_eq!(all.len(), 8, "{all:?}");
        assert_eq!(flagged, ["crypto"]);
        assert_eq!(effect_by_name("sigs"), Some(Effect::SavesNotes));
        assert_eq!(effect_by_name("crypto"), Some(Effect::ReadOnly));
        assert_eq!(effect_by_name("nosuchop"), None);
    }
}
