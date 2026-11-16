//! The facts the tools give when the model meets a compiled program (`read`, `grep`, `ls`, `glob`, and the hint
//! lines of the agent): what the file is, from its own bytes (see [`nexo_bin::Compiled`]), and the call of
//! `nexo bin` that reads it, written the way the session can make it.

use nexo_bin::Compiled;
use nexo_core::ToolContext;

/// A path as a call writes it. On Windows the separator is `/`, which every Windows program and the shell there
/// take, so a name never carries a backslash the call languages would read as an escape; elsewhere a backslash is
/// a character of the name and stays (the callers then leave such a name out).
pub fn slash(w: &str) -> String {
    if cfg!(windows) {
        w.replace('\\', "/")
    } else {
        w.to_string()
    }
}

/// A word as the command language of the `debug` and `bin` tools reads it, and as a shell would: double quotes
/// around one with anything but plain characters. Callers keep `"`, `\`, `$` and backticks out of what they quote.
pub fn quoted(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c)) {
        w.to_string()
    } else {
        format!("\"{w}\"")
    }
}

/// A word as a shell reads it: single quotes around one with anything but plain characters.
pub fn shell_quoted(w: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c);
    if !w.is_empty() && w.chars().all(plain) {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// `tool {"command": "<command>"}`: the call of a tool as the hint lines write it.
pub fn tool_call(tool: &str, command: &str) -> String {
    format!("{tool} {{\"command\": \"{}\"}}", command.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Whether a word can be written into a call as [`quoted`] writes it: one with `"`, `\`, `$`, a backtick or a
/// line break cannot, as no quoting carries it the same through the call language and a shell.
pub fn callable(w: &str) -> bool {
    !w.chars().any(|c| matches!(c, '"' | '\\' | '$' | '`' | '\n' | '\r'))
}

/// The call of `nexo bin` that runs `command` (`overview prog`): the `bin` tool's when the session offers it, the
/// shell's otherwise.
pub fn bin_call(command: &str, tool: bool) -> String {
    if tool {
        tool_call("bin", command)
    } else {
        format!("nexo bin {command}")
    }
}

/// Whether the session offers the `bin` tool.
pub fn bin_offered(ctx: &ToolContext) -> bool {
    ctx.tool_names.iter().any(|t| t == "bin")
}

/// `<facts>; <call> reads it`: what the compiled file the model named `file` is, and the call that reads it
/// first. Only the facts when the name cannot be written into a call.
pub fn about(kind: &Compiled, size: Option<u64>, file: &str, tool: bool) -> String {
    let facts = kind.facts(size);
    let file = slash(file);
    if !callable(&file) {
        return facts;
    }
    format!("{facts}; {} reads it", bin_call(&format!("{} {}", kind.op, quoted(&file)), tool))
}

/// A listing (`ls`, `glob`) tells the kind of its files only when it shows at most this many: each one told costs
/// an open and a read of a few KiB, and a longer listing is skimmed for its names.
pub const KINDS_UP_TO: usize = 100;

/// Whether a listed file may be a compiled program, from what the listing already holds: its name and, on Unix,
/// its execute bits. Only such files are opened to tell their kind.
pub fn may_be_program(name: &str, meta: &std::fs::Metadata) -> bool {
    const EXTS: [&str; 12] = ["exe", "dll", "sys", "so", "dylib", "node", "pyd", "out", "elf", "class", "jar", "wasm"];
    if !meta.is_file() {
        return false;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    ext.is_some_and(|e| EXTS.contains(&e.as_str())) || name.contains(".so.") || executable(meta)
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Windows has no execute bit: the name says it.
#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// What the listed file at `path` is, when it is a compiled program: `ELF64 x86-64 Linux executable, stripped`.
pub fn listed_kind(path: &std::path::Path) -> Option<String> {
    nexo_bin::kind_at(path).map(|k| k.facts(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_is_the_tools_when_offered_and_a_name_it_cannot_carry_gets_the_facts_alone() {
        let k = nexo_bin::kind_of(&nexo_bin::fixtures::elf64(false).bytes).unwrap();
        assert_eq!(
            about(&k, Some(2048), "my prog", true),
            "ELF64 x86-64 executable, 2.0 KiB, with symbols; bin {\"command\": \"overview \\\"my prog\\\"\"} reads it"
        );
        assert_eq!(
            about(&k, None, "a.out", false),
            "ELF64 x86-64 executable, with symbols; nexo bin overview a.out reads it"
        );
        assert_eq!(about(&k, None, "a$b", false), "ELF64 x86-64 executable, with symbols");
    }
}
