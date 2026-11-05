//! The benchmark's prompts and its test-output cleaning, ported from aider's benchmark runner
//! (`benchmark/prompts.py` and `run_test_real` / `cleanup_test_output` in `benchmark/benchmark.py`,
//! github.com/Aider-AI/aider). The texts are the benchmark's protocol: they are not reworded.

use regex::Regex;

/// `instructions_addendum` of `benchmark/prompts.py`, with `{file_list}` left in.
pub const INSTRUCTIONS_ADDENDUM: &str = "
####

Use the above instructions to modify the supplied files: {file_list}
Don't change the names of existing functions or classes, as they may be referenced from other code like unit tests, etc.
Only use standard libraries, don't suggest installing any packages.
";

/// `test_failures` of `benchmark/prompts.py`, with `{file_list}` left in.
pub const TEST_FAILURES: &str = "
####

See the testing errors above.
The tests are correct, don't try and change them.
Fix the code in {file_list} to resolve the errors.
";

/// What the benchmark says when the tests do not finish in time (`run_test_real`).
pub const TIMED_OUT: &str = "Tests timed out!";

/// The system prompt of the agent. Aider's protocol has no such text (the model edits the files named in
/// its chat by replying with edits); an agent with tools needs to be told where it is.
pub const SYSTEM: &str =
    "You are a programmer solving one coding exercise. The current directory holds the exercise's files; \
edit the files the task names in place. You can run commands to try your code. The exercise's own tests are not \
here, so check your work with your own small programs, and delete anything you add that the exercise does not need. \
There is no network: use only the language's standard library. When the solution is complete, stop.
";

/// The first message: the exercise's introduction, instructions and appended instructions (those that exist),
/// then the addendum naming `file_list`.
pub fn first_prompt(introduction: Option<&str>, instructions: &str, append: Option<&str>, file_list: &str) -> String {
    let mut s = String::new();
    s.push_str(introduction.unwrap_or(""));
    s.push_str(instructions);
    s.push_str(append.unwrap_or(""));
    s.push_str(&INSTRUCTIONS_ADDENDUM.replace("{file_list}", file_list));
    s
}

/// Python's `str.splitlines` for the separators test output has.
fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            b'\r' => {
                out.push(&s[start..i]);
                if b.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The second message: the test output as the benchmark sends it (lines rejoined with `\n`), then the
/// `test_failures` text.
pub fn retry_prompt(errors: &str, file_list: &str) -> String {
    let mut s = splitlines(errors).join("\n");
    s.push_str(&TEST_FAILURES.replace("{file_list}", file_list));
    s
}

/// `cleanup_test_output`: timing such as `in 0.12s` is removed (it would randomise the reply), and the
/// test directory's full path is replaced by its name.
pub fn cleanup_output(output: &str, testdir: &std::path::Path) -> String {
    static TIMING: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = TIMING.get_or_init(|| Regex::new(r"\bin \d+\.\d+s\b").expect("a fixed pattern"));
    let res = re.replace_all(output, "");
    let name = testdir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    res.replace(&testdir.to_string_lossy().into_owned(), &name)
}

/// The most test output kept: the benchmark keeps all of it, this only guards a runaway program. Head and
/// tail are kept, with a note between.
pub const OUTPUT_CAP: usize = 100_000;

/// `output` as is when short, else its start and end around a note of how much was left out.
pub fn cap_output(output: &str) -> String {
    if output.len() <= 2 * OUTPUT_CAP {
        return output.to_string();
    }
    let mut head = OUTPUT_CAP;
    while !output.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = output.len() - OUTPUT_CAP;
    while !output.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n[... {} bytes of test output left out ...]\n{}", &output[..head], tail - head, &output[tail..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_first_prompt_is_the_ported_text() {
        let p = first_prompt(Some("# Intro\n"), "# Instructions\nDo it.\n", Some("# Append\n"), "a.py b.py");
        assert_eq!(
            p,
            "# Intro\n# Instructions\nDo it.\n# Append\n\n####\n\nUse the above instructions to modify the supplied files: a.py b.py\n\
             Don't change the names of existing functions or classes, as they may be referenced from other code like unit tests, etc.\n\
             Only use standard libraries, don't suggest installing any packages.\n"
        );
        let bare = first_prompt(None, "X", None, "f.rs");
        assert!(bare.starts_with("X\n####\n\nUse the above"), "{bare:?}");
    }

    #[test]
    fn the_retry_prompt_is_the_output_then_the_ported_text() {
        let p = retry_prompt("FAILED test_a\nassert 1 == 2\n", "lib.rs");
        assert_eq!(
            p,
            "FAILED test_a\nassert 1 == 2\n####\n\nSee the testing errors above.\nThe tests are correct, don't try and change them.\n\
             Fix the code in lib.rs to resolve the errors.\n"
        );
        // lines are rejoined: carriage returns and the final newline go
        assert!(retry_prompt("a\r\nb\rc\n\n", "x").starts_with("a\nb\nc\n\n####"));
        assert!(retry_prompt(TIMED_OUT, "x").starts_with("Tests timed out!\n####"));
    }

    #[test]
    fn cleaned_output_has_no_timing_and_no_full_path() {
        let dir = Path::new("/runs/work/python-bob");
        let raw = "===== 3 failed, 2 passed in 0.12s =====\nFile \"/runs/work/python-bob/bob.py\", line 3\nTook in 12.5s and 3 in 4s\n";
        let c = cleanup_output(raw, dir);
        assert_eq!(c, "===== 3 failed, 2 passed  =====\nFile \"python-bob/bob.py\", line 3\nTook  and 3 in 4s\n");
    }

    #[test]
    fn a_runaway_output_is_cut_in_the_middle_only() {
        let short = "x".repeat(2 * OUTPUT_CAP);
        assert_eq!(cap_output(&short), short);
        let long = format!("{}{}{}", "a".repeat(OUTPUT_CAP), "é".repeat(50), "z".repeat(OUTPUT_CAP + 10));
        let c = cap_output(&long);
        assert!(
            c.starts_with("aaa") && c.ends_with("zzz") && c.contains("bytes of test output left out"),
            "{}",
            &c[..40]
        );
        assert!(c.len() < long.len());
    }
}
