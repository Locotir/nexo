use super::clap_facts::*;
use super::shell::*;
use super::*;

async fn call(cwd: &std::path::Path, command: &str) -> ToolOutput {
    Bin.run(json!({"command": command}), &ToolContext::new(cwd.to_path_buf())).await
}

/// An ELF or PE `nexo bin` opens: this test program.
fn this_binary() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

#[test]
fn it_is_a_read_tool_and_every_request_carries_only_the_short_text() {
    let mut set = nexo_core::tool::ToolSet::new();
    set.add(std::sync::Arc::new(Bin));
    let spec = set.specs_for(false).pop().unwrap();
    let sent = serde_json::to_string(
        &json!({"name": spec.name, "description": spec.description, "parameters": spec.parameters}),
    )
    .unwrap();
    eprintln!("bin tool as offered: {} chars: {sent}", sent.len());
    // 298 chars before the text named `debug` (+12)
    assert!(sent.len() <= 310, "{} chars: {sent}", sent.len());
    assert!(Bin.rare() && Bin.kind() == ToolKind::Read);
    assert!(Bin.spec().description.len() > spec.description.len(), "the full text lists the ops");
    // the short text names the ops a model reaches for first
    for w in ["overview", "funcs", "fn", "decomp", "xref", "strings", "objdump", "--help"] {
        assert!(spec.description.contains(w), "{w}");
    }
}

#[tokio::test]
async fn a_relative_file_is_read_from_the_session_folder_and_the_shell_form_works_too() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    for c in ["info prog", "info ./prog", "nexo bin info prog", "bin info prog"] {
        let o = call(dir.path(), c).await;
        assert!(!o.is_error, "{c}: {}", o.content);
        let low = o.content.to_lowercase();
        assert!(low.contains("elf") || low.contains("mach-o") || o.content.contains("PE"), "{c}: {}", o.content);
    }
    let o = call(dir.path(), "overview prog --lines 12").await;
    assert!(!o.is_error, "{}", o.content);
    let missing = call(dir.path(), "info nothere").await;
    assert!(missing.is_error, "{}", missing.content);
}

fn kind(command: &str) -> ToolKind {
    Bin.call_kind(&json!({ "command": command }))
}

/// Every op reads or keeps names, notes and types in nexo's data folder (a read, as `debug`'s `name` and `note`
/// are); the ops that write a file the person named are a write. A script is as strong as its strongest op.
#[test]
fn each_op_has_the_kind_of_what_it_does() {
    for c in [
        "overview prog",
        "info prog",
        "funcs prog --grep x",
        "fn prog main",
        "decomp prog main",
        "xref prog main",
        "calls prog main",
        "cfg prog main",
        "cfg prog main --grep loop --lines 20",
        "strings prog",
        "find prog 90",
        "bytes prog 0x1000 8",
        "names prog",
        "name prog 0x1000 mine",
        "name prog 0x1000 --clear",
        "note prog 0x1000 a note",
        "proto prog main 'int main(void)'",
        "struct prog Item 0:i32:id",
        "var prog main a1 count",
        "crypto prog --save",
        "sigs",
        "sigs --all",
        "nexo bin name prog 0x1000 mine",
        "help",
        "--help",
        "frobnicate x",
        "info 'unclosed",
    ] {
        assert_eq!(kind(c), ToolKind::Read, "{c}");
    }
    for c in [
        "patch prog --at 0x1000 --nop 1 --out prog2",
        "bin patch prog --fn main --bytes 90 --out prog2",
        "sigs --all --out s.sig",
        "info prog; patch prog --at 0x1000 --nop 1 --out prog2",
        "patch prog --at 0x1000 --nop 1 --out prog2\ninfo prog",
    ] {
        assert_eq!(kind(c), ToolKind::Write, "{c}");
    }
    // a `;` inside quotes is text, not a second op
    assert_eq!(kind("note prog 0x1 'x; patch prog --at 1 --nop 1 --out q'"), ToolKind::Read);
    assert_eq!(Bin.call_kind(&json!({})), ToolKind::Read);
    assert_eq!(Bin.kind(), ToolKind::Read);
}

/// What the person is asked, from the kind the tool reports, under each mode of the real policy.
#[test]
fn a_patch_is_asked_about_like_a_file_edit_and_the_naming_ops_are_not() {
    use nexo_agent::{Decision, Policy};
    use nexo_core::permission::PermissionMode;
    let cwd = std::path::Path::new("/work/proj");
    let decide = |mode, command: &str| {
        let args = json!({ "command": command });
        Policy { mode, ..Default::default() }.decide("bin", Bin.call_kind(&args), &args, cwd)
    };
    let patch = "patch prog --at 0x1000 --nop 1 --out prog2";
    // the same answers as `edit` of a file, in the default mode and where only reads run
    let edit =
        |mode| Policy { mode, ..Default::default() }.decide("edit", ToolKind::Write, &json!({"path": "prog2"}), cwd);
    assert_eq!(decide(PermissionMode::Ask, patch), Decision::Ask);
    assert_eq!(decide(PermissionMode::Ask, patch), edit(PermissionMode::Ask));
    assert!(matches!(decide(PermissionMode::ReadOnly, patch), Decision::Deny(_)));
    assert!(matches!(decide(PermissionMode::Plan, patch), Decision::Deny(_)));
    // a mode that lets writes through does not ask
    assert_eq!(decide(PermissionMode::Auto, patch), Decision::Allow);
    // a patch in a script asks for the script
    assert_eq!(decide(PermissionMode::Ask, &format!("info prog; {patch}")), Decision::Ask);
    // an allow rule for the tool covers it
    let args = json!({ "command": patch });
    let ruled = Policy { allow: vec!["bin".into()], ..Default::default() };
    assert_eq!(ruled.decide("bin", Bin.call_kind(&args), &args, cwd), Decision::Allow);
    // what only reads or names things is never asked, in the default mode, and read-only mode allows it
    for c in ["info prog", "name prog 0x1000 mine", "note prog 0x1000 hi", "proto prog main 'int f(void)'", "sigs"] {
        for mode in [PermissionMode::Ask, PermissionMode::ReadOnly] {
            assert_eq!(decide(mode, c), Decision::Allow, "{c}");
        }
    }
}

/// A rule that denies writing the place stops the patch, and nothing is written; the ask is the turn's, so a
/// rule that only asks does not ask a second time.
#[tokio::test]
async fn a_patch_writes_the_copy_in_the_session_folder_unless_a_rule_denies_the_place() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let patch = "patch prog --at 0x1000 --nop 1 --out prog2";
    let ctx = rules(&["edit(**/prog2)"], dir.path());
    let o = Bin.run(json!({ "command": patch }), &ctx).await;
    assert!(o.is_error && o.content.contains("denies writing"), "{}", o.content);
    assert!(!dir.path().join("prog2").exists());
    let o = Bin.run(json!({ "command": patch }), &ToolContext::new(dir.path().to_path_buf())).await;
    // 0x1000 is in this test program's headers or code: either the copy exists or the answer says why not
    if o.is_error {
        eprintln!("patch refused by nexo-bin: {}", o.content);
        assert!(!o.content.contains("refused"), "{}", o.content);
    } else {
        assert!(dir.path().join("prog2").is_file(), "{}", o.content);
    }
}

#[tokio::test]
async fn two_ops_in_one_call_answer_in_order_each_under_its_own_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let o = call(dir.path(), "info prog; strings prog --lines 3").await;
    assert!(!o.is_error, "{}", o.content);
    let (a, b) = (o.content.find("> info prog").unwrap(), o.content.find("> strings prog --lines 3").unwrap());
    assert!(a < b, "{}", o.content);
    let alone = call(dir.path(), "info prog").await.content;
    assert!(o.content[a..b].contains(alone.lines().next().unwrap()), "{}", o.content);
    assert!(!o.content.contains("script stopped"), "{}", o.content);
    // one op per line is the same call, and one op alone has no header, as before
    let lines = call(dir.path(), "info prog\nstrings prog --lines 3").await;
    assert_eq!(lines.content, o.content);
    assert!(!alone.contains("> info"), "{alone}");
    assert_eq!(call(dir.path(), "info prog;").await.content, alone);
}

/// An error is reported in its place with its own command, and the ops after it still run.
#[tokio::test]
async fn an_error_in_a_script_is_reported_and_the_rest_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let o = call(dir.path(), "info nothere; info prog; info prog").await;
    assert!(!o.is_error, "{}", o.content);
    assert!(o.content.starts_with("> info nothere\nerror: bin: "), "{}", o.content);
    assert!(!o.content.contains("script stopped"), "{}", o.content);
    assert_eq!(o.content.matches("> info prog").count(), 2, "{}", o.content);
    let last = call(dir.path(), "info prog; frobnicate x").await;
    assert!(
        last.content.contains("> info prog") && last.content.contains("> frobnicate x\nerror: "),
        "{}",
        last.content
    );
}

/// One `error:` prefix, a shell word gets a fact, a near-miss names the real op, and the rest of the script runs.
#[tokio::test]
async fn a_word_that_is_not_an_op_gets_a_fact_and_the_script_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let o = call(dir.path(), "info prog; echo ===; info prog").await;
    assert!(!o.content.contains("error: error:"), "{}", o.content);
    assert!(
        o.content.starts_with("note: results already come one per op, so a separator (`echo ===`) is not needed")
            && o.content.contains("> echo ===\nnot run: a shell command"),
        "{}",
        o.content
    );
    assert!(!o.content.contains("similar subcommand"), "{}", o.content);
    assert_eq!(o.content.matches("> info prog").count(), 2, "{}", o.content);
    for w in ["disasm prog main", "objdump prog", "disassemble prog main"] {
        let d = call(dir.path(), &format!("info prog; {w}")).await;
        assert!(d.content.contains("`fn ./prog <name or addr>` shows the disassembly"), "{}", d.content);
    }
    let one = call(dir.path(), "frobnicate x").await;
    assert!(one.is_error && !one.content.starts_with("error:"), "{}", one.content);
}

/// t2d: `./calc 3 7 5` in a script was read as an op and got `tip: a similar subcommand exists: 'calls'`.
#[tokio::test]
async fn a_program_path_is_said_to_be_taken_as_an_op() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let o = call(dir.path(), "info prog; ./prog 3 7 5").await;
    assert!(
        o.content.contains("`./prog` is a program path, and the first word of an op line is read as the op name"),
        "{}",
        o.content
    );
    assert!(o.content.contains("`overview ./prog`") && o.content.contains("`run ./prog ARGS`"), "{}", o.content);
    assert!(!o.content.contains("similar subcommand"), "{}", o.content);
}

/// t1: `symbols` got no hint; now `syms` is the op that lists the symbol tables.
#[test]
fn an_unknown_op_names_the_close_real_op_or_lists_them() {
    assert!(unknown_op("function").contains("`funcs`"), "{}", unknown_op("function"));
    assert!(unknown_op("decompile").contains("`decomp`"), "{}", unknown_op("decompile"));
    assert!(unknown_op("string").contains("`strings`"), "{}", unknown_op("string"));
    assert!(unknown_op("symbols").contains("`syms`"), "{}", unknown_op("symbols"));
    let far = unknown_op("zzzzqq");
    assert!(
        far.starts_with("`zzzzqq` is not a bin op. The ops are: ") && far.contains("overview") && far.contains("funcs"),
        "{far}"
    );
    assert!(!far.contains("did you mean"), "{far}");
}

#[tokio::test]
async fn an_unknown_flag_says_which_flags_the_op_takes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let o = call(dir.path(), "fn prog main --asm").await;
    assert!(o.is_error, "{}", o.content);
    assert!(
        o.content.contains("`fn` takes: ") && o.content.contains("--bytes") && o.content.contains("--to"),
        "{}",
        o.content
    );
    assert!(!o.content.starts_with("error:"), "{}", o.content);
    // clap's usage block and its `-- --asm` tip are left out: the first line, then nexo's own
    assert_eq!(o.content.lines().count(), 2, "{}", o.content);
    assert!(o.content.starts_with("unexpected argument '--asm' found\n`fn` takes: "), "{}", o.content);
    assert!(!o.content.contains("tip:") && !o.content.contains("Usage"), "{}", o.content);
}

const T1D: &str = "/tmp/claude-1000/-home-lctr/0813d0c7-a644-4a6f-8cd3-c8c257f6635e/scratchpad/re-t5";

/// t1d: `> funcs` printed clap's whole block (twice in one call).
#[tokio::test]
async fn a_missing_argument_gets_the_first_line_and_the_ops_usage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("paint")).unwrap();
    let o = call(dir.path(), "decomp ./paint 0x1189; funcs; strings").await;
    assert!(
        o.content.contains(
            "> funcs\nerror: the following required arguments were not provided\nusage: funcs ./prog [--grep RE]"
        ),
        "{}",
        o.content
    );
    assert!(
        o.content.contains(
            "> strings\nerror: the following required arguments were not provided\nusage: strings ./prog [--grep RE]"
        ),
        "{}",
        o.content
    );
    assert!(!o.content.contains("For more information") && !o.content.contains("<FILE>"), "{}", o.content);
    let one = call(dir.path(), "fn ./paint").await;
    assert!(
        one.is_error && one.content.contains("usage: fn ./prog <name|addr> [--bytes] [--to ADDR]"),
        "{}",
        one.content
    );
}

/// t1d: `decomp ./paint 0x1170 2>/dev/null | head -1` blamed a flag of decomp.
#[tokio::test]
async fn a_pipe_and_a_redirect_are_named_and_the_op_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("paint")).unwrap();
    let o = call(dir.path(), "info ./paint; info ./paint 2>/dev/null | head -1").await;
    assert!(!o.is_error, "{}", o.content);
    assert_eq!(o.content.matches("note: ").count(), 1, "{}", o.content);
    let note = o.content.lines().next().unwrap();
    assert!(note.starts_with("note: `2>` and `|` are shell; bin runs no shell"), "{note}");
    assert!(note.contains("output is already bounded"), "{note}");
    assert!(o.content.contains("> info ./paint\n") && !o.content.contains("unexpected argument"), "{}", o.content);
    // the op before the pipe ran
    assert_eq!(o.content.matches("> info ./paint").count(), 2, "{}", o.content);
    // the flags named are the ones the op has
    let d = call(dir.path(), "decomp ./paint 0x1170 2>/dev/null | head -1").await;
    let note = d.content.lines().next().unwrap();
    assert!(note.contains("use `--lines N` after the op instead of `| head`"), "{note}");
    let g = call(dir.path(), "funcs ./paint | grep main").await;
    assert!(g.content.contains("use `--grep RE` after the op instead of `| grep`"), "{}", g.content);
    // an operator inside a word is text
    let r = call(dir.path(), "funcs ./paint --grep main|init").await;
    assert!(!r.content.contains("note:") && !r.content.contains("not run"), "{}", r.content);
}

/// t3b: `cd <dir> && nexo bin decomp ...; echo ---; ...` and t3d: `c=<path>; bin funcs $c; ...`.
#[tokio::test]
async fn cd_and_variables_are_not_run_and_the_real_ops_are() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("names")).unwrap();
    let o = call(dir.path(), &format!("cd {T1D}/t3b && nexo bin info ./names; echo ---; nexo bin info ./names")).await;
    assert!(!o.is_error, "{}", o.content);
    assert_eq!(o.content.matches("note: ").count(), 1, "{}", o.content);
    assert!(o.content.starts_with("note: `cd` and `&&` are shell; bin runs no shell"), "{}", o.content);
    assert!(
        o.content.contains("results already come one per op, so a separator (`echo ---`) is not needed"),
        "{}",
        o.content
    );
    assert!(
        !o.content.contains("`echo`") && !o.content.contains("cd is a shell command, not a bin op;"),
        "{}",
        o.content
    );
    assert!(o.content.contains(&format!("> cd {T1D}/t3b\nnot run: a shell command")), "{}", o.content);
    assert_eq!(o.content.matches("> nexo bin info ./names\n").count(), 2, "{}", o.content);
    assert!(!o.content.contains("unrecognized subcommand"), "{}", o.content);

    let v = call(dir.path(), &format!("c={T1D}/t3d/./names; bin info $c; echo ---; bin info $c")).await;
    assert_eq!(v.content.matches("note: ").count(), 1, "{}", v.content);
    assert!(
        v.content.contains("`c`, `$c` not expanded: write the path in each op")
            || v.content.contains("`c` and `$c`")
            || v.content.contains("`$c`"),
        "{}",
        v.content
    );
    assert!(!v.content.contains("cannot read") && !v.content.contains("unrecognized subcommand"), "{}", v.content);
    assert!(v.content.contains("> bin info $c\nnot run: `$c` is not expanded, write the path"), "{}", v.content);
    // `cd` and `true` are shell, not separators
    assert!(
        not_an_op("cd", &[]).unwrap().contains("bin runs no shell")
            && !not_an_op("cd", &[]).unwrap().contains("separator")
    );
    assert!(not_an_op("true", &[]).unwrap().contains("bin runs no shell"));
    assert!(not_an_op("echo", &[]).unwrap().contains("separator"));
}

/// The names other tools give a file's tables lead to the `syms` call that lists them, written with the file.
#[test]
fn table_words_from_other_tools_lead_to_syms_with_the_file() {
    let rest = |l: &str| l.split(' ').map(String::from).collect::<Vec<_>>();
    let fact = |w: &str, l: &str| not_an_op(w, &rest(l)).unwrap();
    assert!(fact("imports", "./calc").contains("`syms ./calc --kind import`"), "{}", fact("imports", "./calc"));
    assert!(fact("exports", "--json ./lib.so").contains("`syms ./lib.so --kind export`"));
    assert!(fact("nm", "-D ./calc").contains("`syms ./calc`"));
    assert!(fact("dynsym", "./calc").contains("`syms ./calc --kind import,export`"));
    assert!(
        fact("readelf", "-a ./calc").contains("`syms ./calc --kind all`")
            && fact("readelf", "./calc").contains("`info ./calc`")
    );
    assert!(fact("sections", "./calc").contains("--kind section"));
    assert!(fact("relocs", "./calc").contains("--kind reloc"));
    for w in ["got", "plt", "iat"] {
        assert!(fact(w, "./calc").contains("`syms ./calc --kind got`"), "{w}");
    }
    assert!(
        fact("ldd", "./calc").contains("`syms ./calc --kind needed`")
            && fact("ldd", "./calc").contains("without running")
    );
    assert!(not_an_op("libs", &[]).unwrap().contains("`syms ./prog --kind needed`"));
    let spaced = not_an_op("imports", &["my prog".to_string()]).unwrap();
    assert!(spaced.contains("`syms 'my prog' --kind import`"), "{spaced}");
    // the same fact comes back from the tool, whose op line is parsed by clap's rules
    let e = crate::cli::Args::try_parse_from(["nexo", "bin", "imports", "./calc"]).unwrap_err();
    assert!(bin_error(&e, &rest("imports ./calc")).unwrap().contains("`syms ./calc --kind import`"));
}

/// `nexo bin disasm ./calc main` and `nexo bin fn ./calc main --asm` say what the tool says.
#[test]
fn the_command_line_gets_the_same_facts_as_the_tool() {
    let words = |l: &str| l.split(' ').map(String::from).collect::<Vec<_>>();
    let err = |l: &str| {
        let argv = [vec!["nexo".to_string(), "bin".to_string()], words(l)].concat();
        let e = crate::cli::Args::try_parse_from(argv).unwrap_err();
        bin_error(&e, &words(l))
    };
    let d = err("disasm ./calc main").unwrap();
    assert_eq!(d, not_an_op("disasm", &[]).unwrap());
    assert!(d.contains("`fn ./prog <name or addr>` shows the disassembly"), "{d}");
    let f = err("fn ./calc main --asm").unwrap();
    assert!(f.starts_with("unexpected argument '--asm' found\n`fn` takes: "), "{f}");
    assert!(!f.contains("tip:") && !f.contains("Usage"), "{f}");
    // a word nexo has no fact for: the real op names
    let g = err("zzzzqq ./calc").unwrap();
    assert!(g.starts_with("`zzzzqq` is not a bin op") && !g.contains("Usage"), "{g}");
    // a missing argument: clap's first line and the op's usage line, not its block
    let m = err("fn ./calc").unwrap();
    assert!(m.starts_with("the following required arguments were not provided\nusage: fn ./prog <name|addr>"), "{m}");
    assert!(!m.contains("For more information"), "{m}");
    // an error that is not about an op, a flag or an argument stays clap's
    assert!(err("fn ./calc main --to").is_none());
}

#[test]
fn a_semicolon_in_quotes_is_text_and_not_the_end_of_an_op() {
    assert_eq!(units("info a; info b"), ["info a", "info b"]);
    assert_eq!(units("note p 0x1 'a; b'"), ["note p 0x1 'a; b'"]);
    assert_eq!(units("note p 0x1 \"a; b\"; info p"), ["note p 0x1 \"a; b\"", "info p"]);
    assert_eq!(units("note p 0x1 'a; b'; info p"), ["note p 0x1 'a; b'", "info p"]);
    assert!(units("# only a comment").is_empty());
}

/// The rules of a session: a real policy, asked the way the turn asks it.
fn rules(deny: &[&str], cwd: &std::path::Path) -> ToolContext {
    let policy = nexo_agent::Policy { deny: deny.iter().map(|d| d.to_string()).collect(), ..Default::default() };
    let rules_cwd = cwd.to_path_buf();
    let mut ctx = ToolContext::new(cwd.to_path_buf());
    ctx.check = Some(std::sync::Arc::new(move |tool: &str, kind, args: &Value| {
        match policy.decide(tool, kind, args, &rules_cwd) {
            nexo_agent::Decision::Allow => Check::Allow,
            nexo_agent::Decision::Ask => Check::Ask,
            nexo_agent::Decision::Deny(_) => Check::Deny,
        }
    }));
    ctx
}

#[tokio::test]
async fn the_rules_that_cover_read_cover_the_files_named_inside_the_command() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("locked")).unwrap();
    std::fs::copy(this_binary(), dir.path().join("locked").join("prog")).unwrap();
    std::fs::copy(this_binary(), dir.path().join("open")).unwrap();
    let ctx = rules(&["read(**/locked/*)"], dir.path());
    // relative, absolute, in the shell form, and as the second file of a diff: all the same file to the rules
    let abs = dir.path().join("locked/prog");
    for c in [
        "info locked/prog".to_string(),
        format!("info {}", abs.display()),
        "nexo bin overview ./locked/prog".to_string(),
        "diff open locked/prog".to_string(),
        "diff locked/prog open".to_string(),
    ] {
        let o = Bin.run(json!({ "command": c }), &ctx).await;
        assert!(o.is_error && o.content.contains("denies reading"), "{c}: {}", o.content);
    }
    // a file the rules do not name is read, with the same context
    let o = Bin.run(json!({"command": "info open"}), &ctx).await;
    assert!(!o.is_error, "{}", o.content);
    // no rules at all (no check): read as before
    let o = call(dir.path(), "info locked/prog").await;
    assert!(!o.is_error, "{}", o.content);
}

#[tokio::test]
async fn a_file_the_rules_ask_about_is_not_read_without_the_person() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(this_binary(), dir.path().join("prog")).unwrap();
    let mut ctx = ToolContext::new(dir.path().to_path_buf());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    ctx.check = Some(std::sync::Arc::new(move |tool: &str, kind, args: &Value| {
        log.lock().unwrap().push((tool.to_string(), kind, args["path"].as_str().unwrap().to_string()));
        Check::Ask
    }));
    let o = Bin.run(json!({"command": "info prog"}), &ctx).await;
    assert!(o.is_error && o.content.contains("needs the person's OK"), "{}", o.content);
    let asked = seen.lock().unwrap().clone();
    assert_eq!(asked, [("read".to_string(), ToolKind::Read, dir.path().join("prog").to_string_lossy().to_string())]);
}

#[tokio::test]
async fn help_a_missing_command_and_a_bad_line_answer_in_words() {
    let dir = tempfile::tempdir().unwrap();
    for c in ["help", "--help", "fn --help"] {
        let o = call(dir.path(), c).await;
        assert!(!o.is_error && !o.content.is_empty(), "{c}: {}", o.content);
    }
    let o = Bin.run(json!({}), &ToolContext::new(dir.path().to_path_buf())).await;
    assert!(o.is_error && o.content.contains("command"), "{}", o.content);
    let o = call(dir.path(), "info 'unclosed").await;
    assert!(o.is_error && o.content.contains("quote"), "{}", o.content);
    let o = call(dir.path(), "frobnicate x").await;
    assert!(o.is_error, "{}", o.content);
    assert_eq!(Bin.summary(&json!({"command": "fn p main"})), "bin: fn p main");
}

/// Both directions on a real program: the `bin` answer names the `debug` call, the call stops where the
/// answer looked, and the stop names the `bin` call that reads the function it is in.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[tokio::test]
async fn a_bin_answer_and_a_debug_stop_name_each_others_calls() {
    use clap::Parser;
    let dir = tempfile::tempdir().unwrap();
    let Some(exe) = crate::bin_cmd::bridge::tests::build(dir.path(), "prog", &["-g0"]) else {
        return eprintln!("skipped: no cc");
    };
    let ctx = ToolContext::new(dir.path().to_path_buf());
    let answer = Bin.run(json!({"command": "fn prog helper"}), &ctx).await;
    assert!(!answer.is_error, "{}", answer.content);
    let last = answer.content.lines().last().unwrap();
    assert_eq!(answer.content.matches("to see it live").count(), 1, "{}", answer.content);
    let call = last
        .strip_prefix("to see it live: debug `")
        .and_then(|l| l.strip_suffix('`'))
        .unwrap_or_else(|| panic!("{last}"));
    // every unit of the call parses in the debugger, and the break is at the address the answer is about
    let helper = match nexo_bin::place_of(&nexo_bin::Image::open(&exe).unwrap(), "helper").unwrap() {
        nexo_bin::Place::Code(a) => a,
        other => panic!("{other:?}"),
    };
    let units = nexo_dbg::script_units(call);
    assert_eq!(units.len(), 3, "{units:?}");
    for u in &units {
        nexo_dbg::parse(&nexo_dbg::split_words(u)).unwrap_or_else(|e| panic!("{u}: {e}"));
    }
    assert_eq!(nexo_dbg::parse(&nexo_dbg::split_words(&units[1])).unwrap().words, ["break", &format!("*{helper:#x}")]);
    assert!(answer.content.contains("helper"), "{}", answer.content);
    // the call stops there, and the stop names the `bin` call for the function
    let state = tempfile::tempdir().unwrap();
    let dbg = nexo_tools::debug::Debug::in_dir(state.path());
    let stop = dbg.run(json!({"command": call}), &ctx).await;
    assert!(!stop.is_error, "{}", stop.content);
    assert!(stop.content.contains("helper"), "{}", stop.content);
    let bridge = stop.content.lines().last().unwrap().to_string();
    let words = bridge.strip_prefix("to read this function: bin ").unwrap_or_else(|| panic!("{}", stop.content));
    let argv: Vec<String> =
        ["nexo", "bin"].iter().map(|s| s.to_string()).chain(crate::re_mcp::split_words(words).unwrap()).collect();
    assert!(matches!(crate::cli::Args::try_parse_from(&argv).unwrap().cmd, Some(crate::cli::Cmd::Bin(_))), "{words}");
    let read = Bin.run(json!({"command": words}), &ctx).await;
    assert!(!read.is_error && read.content.contains("helper"), "{}", read.content);
    // stepping inside the same function does not repeat the line
    let again = dbg.run(json!({"command": "step"}), &ctx).await;
    assert!(!again.content.contains("to read this function"), "{}", again.content);
    dbg.run(json!({"command": "end"}), &ctx).await;
}
