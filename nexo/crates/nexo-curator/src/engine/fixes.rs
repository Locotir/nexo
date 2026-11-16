//! Test runs and their failures: which failed, and which a later run fixed.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use crate::sorted::FxHashMap;
use serde_json::Value;

use crate::hindsight::UKind;
use crate::ledger::ERROR_MARKS;
use crate::msg::key_of;
use crate::textops::{self, Re};
use crate::tools::{shell_writes, Kind};

use super::files::{basename, command_of};
use super::Engine;

pub(super) static TEST_COMMAND: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"\b(?:pytest|py\.test|unittest|tox|nox|jest|vitest|mocha|rspec|ctest|phpunit|nextest)\b|\b(?:cargo|go|npm|pnpm|yarn|bun|deno|dotnet|mix|gradlew?|mvnw?|swift|zig|sbt|lein|bazel|stack|cabal|flutter|dart|rails|rake|artisan|playwright|manage\.py)\s+(?:run\s+)?(?:test|spec)\b|\bnode\s+--test\b|\bmake\s+(?:test|check)\b",
        0,
    )
});

/// A failing test run, in the output shapes the runners of most ecosystems share: a line that starts with a
/// failure word (`--- FAIL:`, `FAIL`, `FAILED`, `FAILURES!`, `Failed!`, `BUILD FAILURE`, `not ok 2`, `✕`, `✖`,
/// `# fail 1`, `ℹ fail 1`, `make: *** ... Error 2`), a count of one or more failures (`1 failed`, `2 failing`,
/// `1 failure`, `1 tests failed`) or a labelled one where a summary prints it (`Failures: 1,`, `Failed: 1`,
/// `errors=1)`, `, Failed 1,`), or a non-zero exit status at a line start (`exit status 1`, `[exit code 1]`) or
/// after `failed with` on a line that is not a passing test's (`✓`, `ok `, `PASS`). A word and a number in a
/// test name, a package path or a log line (`returns error 404`, `api/errors 1.2s`) are not one. A run piped
/// through `tail` exits 0 whatever its tests did.
pub(super) static TEST_FAILED: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*(?:--- FAIL:|FAIL\b|FAILED\b|FAILURES?!|Failed!|(?:\[\w+\]\s+)?BUILD FAIL|not ok \d|[✕✗✖]\s|[#ℹ]\s*fail\s+[1-9]|make(?:\[\d+\])?: \*\*\* .*\bError [1-9])|\b[1-9]\d* (?:tests? )?(?:failed|failing|failures?|errors?)\b(?![ \t]*[:=]?[ \t]*\d)|(?i:(?:^|[,;(\-]|\btests?)[ \t]*(?:failures?|failed|errors?)[ \t]*[:=][ \t]*[1-9]\d*(?=[ \t]*(?:[,.;)]|$))|[,;(][ \t]*(?:failures?|failed|errors?)[ \t]+[1-9]\d*(?=[ \t]*[,;)]))|(?i:^\s*\[?exit (?:status|code)[ \t]*:?[ \t]*[1-9]\d*\b|^(?![ \t]*(?-i:[✓✔√]|ok\s|PASS\b))[^\n]*?\b(?:failed|finished|exited) with (?:exit )?(?:code|status) [1-9]\d*\b)",
        textops::M,
    )
});

static FAILURE_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r#"FAIL|ERROR|Error|Traceback|File "|panicked|fail|assert|✗|✕|●|^\s*not ok\b|\.\w+:\d+"#, textops::M)
});

/// The name of a failing test, in the shapes runners print it: after `::`, `FAIL:`, `--- FAIL:` or `, in`
/// (`test_x`, `TestX`), `test a::b ... FAILED` (its last part), `Class > name() FAILED`, a `●`/`✕` line,
/// `rspec <file>:<line>` and `not ok N - name`. Each alternative has one group; `failed_tests` takes the one
/// that took part.
static FAILED_TEST: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*test\s+(?:[\w$]+::)*([\w$]+)\s+\.\.\.\s+FAILED|^\s*[\w.$]+\s+>\s+([\w$]+)(?:\(\))?\s+FAILED|^\s*[●✕✗]\s+(?!Console\b|Test suite failed)(.+?)(?:\s+\(\d+(?:\.\d+)?\s*m?s\))?\s*$|^\s*rspec\s+(\S+:\d+)|^\s*not ok \d+ - (.+?)\s*$|(?:::|\b(?:FAIL|FAILED|ERROR):?\s+|---\s+FAIL:\s+|,\s+in\s+)(test_\w+|Test[A-Z_]\w*)",
        textops::M,
    )
});

static FAILURE_COUNT: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r"\b(?:failures|errors)=(\d+)|\b(\d+) (?:tests? )?(?:failed|failing|failures?|errors?)\b", 0)
});

/// The failing tests a run's failure lines name (`FAILED_TEST`).
fn failed_tests(f: &str) -> Vec<&str> {
    FAILED_TEST
        .captures_all(f)
        .iter()
        .filter_map(|c| c.iter().skip(1).flatten().next().map(|&(a, b)| &f[a..b]))
        .collect()
}

static TEST_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?:^|[/_.])(?:tests?|spec)(?=[/_.]|$)", 0));

const OUTCOME_CHARS: usize = 700;

fn id_key(v: Option<&Value>) -> String {
    match v {
        None => key_of(Some(&Value::String(String::new()))),
        Some(v) => key_of(Some(v)),
    }
}

fn failure_count(f: &str) -> i64 {
    FAILURE_COUNT
        .captures_all(f)
        .iter()
        .map(|c| {
            let g = c.get(1).copied().flatten().or_else(|| c.get(2).copied().flatten());
            g.and_then(|(a, b)| f[a..b].parse::<i64>().ok()).unwrap_or(0)
        })
        .sum()
}

/// Whether a passing run of ``pass`` speaks for the tests a failing run of ``failed`` ran: the same command, or
/// a broader one (``failed`` is ``pass`` and more arguments).
fn covers(pass: &str, failed: &str) -> bool {
    failed.strip_prefix(pass).is_some_and(|r| r.is_empty() || r.starts_with(' '))
}

/// The test run a shell command makes: its first piece (between `&&`, `||`, `;`, `|`) that runs tests, without
/// redirections, so `cd app && npm test 2>&1 | tail -5` and `npm test` are one run.
fn test_run(command: &str) -> String {
    let joined = command.replace("&&", "\n").replace("||", "\n");
    let Some(piece) = joined.split(['\n', ';', '|']).find(|s| TEST_COMMAND.is_match(s)) else {
        return command.trim().to_string();
    };
    let mut out: Vec<&str> = Vec::new();
    let mut skip = false;
    for t in textops::split_ws(piece) {
        let bare = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if std::mem::take(&mut skip) {
            continue;
        }
        if bare.starts_with(['<', '>']) {
            // `2>&1`, `>out.txt`; a bare `>` takes the next word as its file
            skip = bare.trim_end_matches('&').chars().all(|c| c == '<' || c == '>');
            continue;
        }
        out.push(t);
    }
    out.join(" ")
}

/// The stem of the file at `path`, when it can name a test file of its own (`named_in`).
fn stem_of(path: &str) -> Option<&str> {
    let stem = basename(path).split('.').next().unwrap_or("");
    (!stem.is_empty() && !TEST_NAME.is_match(stem)).then_some(stem)
}

/// The names a test file of `stem` takes (`calc` → `test_calc.py`, `calc_test.go`, `calc.spec.ts`).
fn tested_by(stem: &str) -> Option<Re> {
    let pat = format!(r"(?:\btests?_|\b){}(?:_tests?|[._](?:test|spec))?\.\w+\b", textops::escape(stem));
    Re::try_new(&pat, 0).ok()
}

fn named_by(path: &str, failures: &[String], tested: &Re) -> bool {
    failures.iter().any(|f| f.contains(path) || tested.find_all(f).iter().any(|&(a, b)| TEST_NAME.is_match(&f[a..b])))
}

#[cfg(test)]
pub(super) fn named_in(path: &str, failures: &[String]) -> bool {
    let Some(tested) = stem_of(path).and_then(tested_by) else { return false };
    named_by(path, failures, &tested)
}

/// What `fixes` read of the test runs' outputs, by unit, and the test file names of each file stem it looked
/// for in failures. `fixes` goes over every request the map shows at each model call, so without this a long
/// session matches the same outputs again at every call, and a resumed one at every reply it replays. The units
/// of an engine only grow and a rewritten history starts a new engine, so what is kept of a unit holds while
/// the engine lives; a copy of the engine (a landing tried) takes it along.
#[derive(Clone, Default)]
pub(super) struct RunMemo(RefCell<RunMemoIn>);

#[derive(Clone, Default)]
struct RunMemoIn {
    /// By unit: the `is_error` it was read with, and its failure lines when the run failed (`None`: it passed).
    runs: FxHashMap<usize, (bool, Option<Arc<str>>)>,
    /// By file stem: what a test file of it is called (`None`: no pattern).
    names: FxHashMap<String, Option<Arc<Re>>>,
    /// The outputs matched so far: what the tests bound.
    reads: usize,
}

impl RunMemo {
    /// The failure lines of the test run whose output is unit `i`, or `None` when it passed.
    fn failure<'a>(&self, i: usize, error: bool, text: impl FnOnce() -> Cow<'a, str>) -> Option<Arc<str>> {
        if let Some((e, f)) = self.0.borrow().runs.get(&i) {
            if *e == error {
                return f.clone();
            }
        }
        let text = text();
        let failed = error || ERROR_MARKS.is_match(&text) || TEST_FAILED.is_match(&text);
        let f = failed.then(|| {
            let ls: Vec<&str> = textops::splitlines(&text).into_iter().filter(|x| FAILURE_LINE.is_match(x)).collect();
            Arc::<str>::from(ls.join("\n"))
        });
        let mut m = self.0.borrow_mut();
        m.reads += 1;
        m.runs.insert(i, (error, f.clone()));
        f
    }

    /// `named_in`, with the pattern of each stem made once.
    fn named_in(&self, path: &str, failures: &[String]) -> bool {
        let Some(stem) = stem_of(path) else { return false };
        let known = self.0.borrow().names.get(stem).cloned();
        let tested = match known {
            Some(t) => t,
            None => {
                let t = tested_by(stem).map(Arc::new);
                self.0.borrow_mut().names.insert(stem.to_string(), t.clone());
                t
            }
        };
        tested.is_some_and(|t| named_by(path, failures, &t))
    }

    /// The outputs of test runs matched so far.
    #[cfg(test)]
    pub(super) fn reads(&self) -> usize {
        self.0.borrow().reads
    }
}

impl Engine {
    pub(super) fn outcome(&self, r: usize, end: usize) -> String {
        let last =
            (r + 1..end).rev().find(|&mi| self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant"));
        let Some(last) = last else { return String::new() };
        if last >= self.start || self.fz.by_msg(last).iter().any(|&i| self.tr.units[i].kind == UKind::Call) {
            return String::new();
        }
        let x = textops::squash_ws(&self.unit_text_at(last, UKind::Text));
        if textops::len(&x) > OUTCOME_CHARS {
            format!("{}…", textops::head(&x, OUTCOME_CHARS))
        } else {
            x
        }
    }

    pub(super) fn fixes(&self, r: usize, end: usize, limit: Option<usize>) -> String {
        let mut changed: Vec<String> = Vec::new();
        let mut fixed: Vec<String> = Vec::new();
        let mut tests: Vec<String> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        // each failing run with its test run (`test_run`): only a run of that command, or of a broader one, says
        // whether its tests pass now
        let mut failures: Vec<(String, String)> = Vec::new();
        let mut commands: FxHashMap<String, String> = FxHashMap::default();
        let mut unnamed: i64 = 0;
        let stop = end.min(limit.unwrap_or(self.start));
        for mi in r..stop {
            for &i in self.fz.by_msg(mi) {
                let (kind, path, error) = match self.fz.info.get(&i) {
                    Some(x) => (Some(x.kind), x.path.clone(), x.error),
                    None => (None, None, false),
                };
                let b = self.tr.block(i);
                match self.tr.units[i].kind {
                    UKind::Call => {
                        let command = if kind == Some(Kind::Exec) { command_of(b) } else { String::new() };
                        commands.insert(id_key(b.get("id")), command.clone());
                        let ws = match (kind, path) {
                            (Some(Kind::Write), Some(p)) if !p.is_empty() => vec![p],
                            _ => shell_writes(&command),
                        };
                        for w in ws {
                            if !changed.contains(&w) {
                                changed.push(w.clone());
                            }
                            if !failures.is_empty() && !pending.contains(&w) {
                                pending.push(w);
                            }
                        }
                    }
                    UKind::Result if kind == Some(Kind::Exec) => {
                        let cmd = commands.get(&id_key(b.get("tool_use_id"))).map_or("", |s| s.as_str());
                        if !TEST_COMMAND.is_match(cmd) {
                            continue;
                        }
                        let cmd = test_run(cmd);
                        let cmd = cmd.as_str();
                        if let Some(f) = self.run_memo.failure(i, error, || self.tr.unit_text(i)) {
                            let f = f.to_string();
                            // a test failing again no longer passes
                            let again = failed_tests(&f);
                            tests.retain(|t| !again.contains(&t.as_str()));
                            failures.push((cmd.to_string(), f));
                        } else {
                            let (done, rest): (Vec<_>, Vec<_>) = failures.drain(..).partition(|(c, _)| covers(cmd, c));
                            failures = rest;
                            if done.is_empty() {
                                continue;
                            }
                            let done: Vec<String> = done.into_iter().map(|(_, f)| f).collect();
                            let named: Vec<String> =
                                pending.iter().filter(|p| self.run_memo.named_in(p, &done)).cloned().collect();
                            let src = if named.is_empty() { &pending } else { &named };
                            for p in src {
                                if !fixed.contains(p) {
                                    fixed.push(p.clone());
                                }
                            }
                            let mut named_here: Vec<String> = Vec::new();
                            for f in &done {
                                for t in failed_tests(f) {
                                    if !named_here.iter().any(|x| x == t) {
                                        named_here.push(t.to_string());
                                    }
                                }
                            }
                            for t in &named_here {
                                if !tests.contains(t) {
                                    tests.push(t.clone());
                                }
                            }
                            for f in &done {
                                unnamed = unnamed.max(failure_count(f) - named_here.len() as i64);
                            }
                            if failures.is_empty() {
                                pending.clear();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut open: Vec<String> = Vec::new();
        if let Some((_, f)) = failures.last() {
            for t in failed_tests(f) {
                open.push(t.to_string());
            }
        }
        open.sort();
        open.dedup();
        if changed.is_empty() && tests.is_empty() && open.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !changed.is_empty() {
            out = format!("\n  files changed: {}", changed[..changed.len().min(12)].join(", "));
            if changed.len() > 12 {
                out += " …";
            }
        }
        if !fixed.is_empty() {
            out +=
                &format!("; the changes that made the failing tests pass: {}", fixed[..fixed.len().min(8)].join(", "));
        }
        if !tests.is_empty() {
            out += if out.is_empty() { "\n  " } else { "; " };
            let mut s = tests.clone();
            s.sort();
            out += &format!("the tests that failed and now pass: {}", s[..s.len().min(16)].join(", "));
            if tests.len() > 16 {
                out += " …";
            }
            if unnamed > 0 {
                out += &format!(" (the runs counted {unnamed} more failing, not named in what was shown)");
            }
        }
        if !open.is_empty() {
            out += if out.is_empty() { "\n  " } else { "; " };
            out += &format!("tests failing in the last run in them: {}", open[..open.len().min(16)].join(", "));
            if open.len() > 16 {
                out += " …";
            }
        }
        out
    }
}
