//! What the permission check takes for a read: commands that could run another program or write a file never pass.

use super::*;
use nexo_core::ToolKind;
use serde_json::json;

fn kind(c: &str) -> ToolKind {
    Bash.call_kind(&json!({ "command": c }))
}

fn all_ask(cs: &[&str]) {
    for c in cs {
        assert_eq!(kind(c), ToolKind::Exec, "{c}");
    }
}

fn all_read(cs: &[&str]) {
    for c in cs {
        assert_eq!(kind(c), ToolKind::Read, "{c}");
    }
}

#[test]
fn a_sed_command_hidden_after_a_regex_address_is_not_a_read() {
    all_ask(&[
        "sed -n '/./e touch pwned #/p' f.txt",
        "sed -n '/x/w out.txt' f.txt",
        "sed -n '/x/W out.txt' f.txt",
        "sed -n '/a\\/e touch x #/p' f.txt",
        "sed -n '/x\\/p' f.txt",
        "sed -n $'/x\\nw out\\n/p' f.txt",
        "sed -n '/a/,/b/,/c/p' f.txt",
    ]);
    all_read(&[
        "sed -n '/fn main/p' f.rs",
        "sed -n '/a/,/b/p' f.rs",
        "sed -n '5,/end/p' f.rs",
        "sed -n -E '/x+|y/p' f",
    ]);
}

#[test]
fn every_sed_script_is_judged_and_a_script_file_is_not_a_read() {
    all_ask(&[
        "sed -n -e p -e 'w W.txt' f.txt",
        "sed -n -e 1p -e '1W out' f.txt",
        "sed -n --expression='w out' 5p",
        "sed -n --expression 'w out' 5p",
        "sed -n -e'w out' 5p",
        "sed -n --expr='w out' 5p",
        "sed -n -f 1p f.txt",
        "sed -n --file=x.sed 5p",
        "sed -n 's/a/b/w out' f.txt",
        "sed -n 's/a/b/e' f.txt",
        "sed -n 's/a/b/p' f.txt",
        "sed -n '1e touch x' f.txt",
        "sed -n 1p f.txt -i",
        "sed -n -s -i 1p f.txt",
        // a later option after the script: sed without reordering (BSD, or POSIXLY_CORRECT) takes the first word
        // as the script, the others as files
        "sed -n 'w out' -e 1p",
        "sed -n 1p -i -- f.txt",
        "sed -n -e",
        "sed -n",
    ]);
    all_read(&[
        "sed -n -e 1p -e 5p f.txt",
        "sed -n --expression=1p f.txt",
        "sed -n -e10,20p f.txt",
        "sed --quiet '$p' f.txt",
        "sed -n p f.txt",
        "sed -n -- 5p -f.txt",
        "sed -n -s 1p a b",
    ]);
}

#[test]
fn rg_that_runs_a_preprocessor_or_any_other_program_is_not_a_read() {
    all_ask(&[
        "rg --pre sh x s.txt",
        "rg --pre=sh x s.txt",
        "rg --pre-glob '*.pdf' x",
        "rg --pre-glob=*.pdf x",
        "rg --hostname-bin sh x",
        "rg --hostname-bin=./h x",
        "rg -z x a.gz",
        "rg -iz x a.gz",
        "rg --search-zip x a.gz",
        "rg -n x . | rg --pre sh y",
    ]);
    all_read(&[
        "rg -n x src",
        "rg --pretty x",
        "rg -p x",
        "rg --no-search-zip x",
        "rg -g '*.rs' -i x",
        "rg -l x | head",
    ]);
}

#[test]
fn file_that_writes_a_compiled_magic_or_unpacks_through_other_programs_is_not_a_read() {
    all_ask(&[
        "file -C -m magic",
        "file --compile -m magic",
        "file --comp -m magic",
        "file -z a.gz",
        "file -bZ a.gz",
        "file --uncompress a.gz",
        "file --uncompress-noreport a.gz",
        "file -p a",
        "file --preserve-date a",
    ]);
    all_read(&["file a.out", "file -b --mime-type a", "file -L a"]);
}

#[test]
fn programs_that_run_others_stay_out_of_the_reads() {
    all_ask(&[
        "find . -name x -exec sh -c 'touch y' \\;",
        "find . -execdir touch y \\;",
        "find . -ok rm {} \\;",
        "find . -fprint out.txt",
        "find . -fls out.txt",
        "cat x | xargs rm",
        "ls | xargs -I{} cat {}",
        "awk 'BEGIN { system(\"touch x\") }'",
        "awk '{ print > \"out\" }' f",
        "less f",
        "LESSOPEN='|sh %s' less f",
        "man -P 'sh -c id' ls",
        "git -c core.pager=sh log",
        "git -c core.sshCommand=sh log",
        "git --exec-path=. log",
        "git log --ext-diff -p",
        "git diff --output=out.txt",
        "git log --show-signature -1",
        "git log --format=%G? -1",
        "GIT_EXTERNAL_DIFF=sh git diff",
        "PAGER=sh git log",
    ]);
    all_read(&["git log --oneline -5", "git diff --stat", "git show HEAD:a.rs", "git status --short"]);
}

#[test]
fn only_a_redirect_to_dev_null_itself_is_dropped() {
    all_ask(&[
        "cat a >/dev/nullx",
        "cat a > /dev/nullx",
        "cat a 2>/dev/null.d",
        "cat a &>/dev/nullx",
        "cat a >/dev/null >out",
        "cat a >out 2>&1",
        "cat a 1>&2",
        // in the middle of the words: only a redirect after them is dropped
        "cat >/dev/null a",
        "cat 2>&1 a",
    ]);
    all_read(&[
        "cat a >/dev/null",
        "cat a 2>/dev/null",
        "cat a &>/dev/null",
        "cat a >>/dev/null",
        "cat a >/dev/null 2>&1",
        "grep x a 2>/dev/null | head",
    ]);
    // the text of a word is never touched: a quoted `>/dev/null` is what grep looks for
    assert_eq!(
        command_words("grep '2>&1 >/dev/null' f"),
        Some(vec![vec!["grep".into(), "2>&1 >/dev/null".into(), "f".into()]])
    );
    assert_eq!(command_words("ls x 2>/dev/null"), Some(vec![vec!["ls".into(), "x".into()]]));
}

#[test]
fn a_debugger_write_asks_whatever_the_line_redirects_or_substitutes() {
    for c in ["nexo dbg attach 1234 >/dev/nullx", "nexo dbg attach 1234 > out.txt", "nexo dbg attach $(pidof prog)"] {
        assert!(dbg_changes(c), "{c}");
    }
    assert!(!dbg_changes("nexo dbg regs 2>/dev/null"));
}
