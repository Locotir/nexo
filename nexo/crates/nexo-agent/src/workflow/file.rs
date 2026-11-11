//! The workflow file: phases of steps, as TOML, and everything checked before anything starts.
//!
//! A workflow is data, not a script: phases run in order, the steps of a phase run together, a step with
//! `each` runs once per item of an earlier step's list, and `votes` runs independent copies of a step. A file
//! that fails any check starts nothing; every problem is reported at once, with its line where there is one.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use toml_edit::{Document, Table};

pub const DEFAULT_PARALLEL: usize = 4;
pub const MAX_PARALLEL: usize = 8;
pub const DEFAULT_AGENTS: usize = 12;
pub const MAX_AGENTS: usize = 50;
pub const DEFAULT_TOKENS: u64 = 400_000;
pub const MAX_VOTES: usize = 5;
/// Items of a list that are taken; the rest are counted in the result, never dropped in silence.
pub const MAX_ITEMS: usize = 50;
pub const MAX_PROMPT: usize = 16 * 1024;
/// The most a workflow file (or the inline text of one) may hold.
pub const MAX_FILE: usize = 64 * 1024;
/// Characters of a phase title and of a description; they reach the question the person answers.
pub const MAX_TITLE: usize = 80;
pub const MAX_DESCRIPTION: usize = 200;
/// How many entries `answers` holds, and the characters of each.
pub const MIN_ANSWERS: usize = 2;
pub const MAX_ANSWERS: usize = 5;
pub const MAX_ANSWER: usize = 24;
/// Problems that are written out; the rest are counted.
pub const MAX_PROBLEMS: usize = 20;

const ROOT_KEYS: &[&str] = &["description", "max_parallel", "max_agents", "max_tokens", "phase"];
const PHASE_KEYS: &[&str] = &["title", "step"];
const STEP_KEYS: &[&str] = &["id", "agent", "model", "prompt", "returns", "each", "votes", "answers"];

/// A workflow, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub max_parallel: usize,
    pub max_agents: usize,
    pub max_tokens: u64,
    pub phases: Vec<Phase>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phase {
    pub title: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    /// A kind the `task` tool knows; none: the task tool's default.
    pub agent: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    /// `returns = "list"`: the report ends with a JSON array of strings.
    pub list: bool,
    /// The step whose list this one runs once per item of.
    pub each: Option<String>,
    pub votes: usize,
    /// The answers a copy may begin its report with (`votes` above 1); empty: no count line.
    pub answers: Vec<String>,
    /// Where the step starts in the file.
    pub line: Option<usize>,
}

/// One thing wrong with a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub line: Option<usize>,
    pub text: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(n) => write!(f, "line {n}: {}", self.text),
            None => f.write_str(&self.text),
        }
    }
}

/// How many sub-agents a workflow starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Those no list can change: the steps without `each`, a copy each.
    pub certain: usize,
    /// The most it may start: a step with `each` counts `MAX_ITEMS` x votes; never above `max_agents`.
    pub at_most: usize,
}

impl Workflow {
    pub fn plan(&self) -> Plan {
        let (mut certain, mut wide) = (0, 0);
        for s in self.phases.iter().flat_map(|p| &p.steps) {
            if s.each.is_some() {
                wide += MAX_ITEMS * s.votes;
            } else {
                certain += s.votes;
            }
        }
        Plan { certain, at_most: (certain + wide).min(self.max_agents) }
    }

    pub fn step(&self, id: &str) -> Option<(usize, &Step)> {
        self.phases.iter().enumerate().find_map(|(i, p)| p.steps.iter().find(|s| s.id == id).map(|s| (i, s)))
    }

    /// Problems with the kinds of sub-agent the steps name (`kinds`: those the task tool has).
    pub fn check_agents(&self, kinds: &[String]) -> Vec<Problem> {
        let mut out = Vec::new();
        for s in self.phases.iter().flat_map(|p| &p.steps) {
            if let Some(a) = s.agent.as_deref().filter(|a| !kinds.iter().any(|k| k == a)) {
                out.push(Problem {
                    line: s.line,
                    text: format!("step `{}`: no sub-agent kind `{a}`; there are: {}", s.id, kinds.join(", ")),
                });
            }
        }
        out
    }
}

/// Collects problems with the line of the place they are about.
struct Problems<'a> {
    raw: &'a str,
    list: Vec<Problem>,
}

impl Problems<'_> {
    fn line(&self, at: Option<Range<usize>>) -> Option<usize> {
        let start = at?.start.min(self.raw.len());
        Some(self.raw.get(..start)?.matches('\n').count() + 1)
    }

    fn add(&mut self, at: Option<Range<usize>>, text: impl Into<String>) {
        let line = self.line(at);
        self.list.push(Problem { line, text: text.into() });
    }
}

/// `[a-z0-9_-]{1,32}`.
pub fn valid_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A key's value as text, or a problem saying what it should be.
fn text_of(p: &mut Problems, t: &Table, key: &str) -> Option<String> {
    let (k, item) = t.get_key_value(key)?;
    match item.as_str() {
        Some(s) => Some(s.to_string()),
        None => {
            p.add(k.span(), format!("`{key}` must be text in quotes"));
            None
        }
    }
}

/// Text for a person that has no control character (an escape sequence could overwrite what the question shows),
/// and at most `max` characters when a limit is given.
fn plain_text(p: &mut Problems, t: &Table, key: &str, what: &str, max: Option<usize>) -> Option<String> {
    let s = text_of(p, t, key)?;
    let at = t.get_key_value(key).and_then(|(k, _)| k.span());
    if s.chars().any(char::is_control) {
        p.add(at, format!("{what} has a control character; use plain text"));
        return None;
    }
    if let Some(max) = max.filter(|m| s.chars().count() > *m) {
        p.add(at, format!("{what} is {} characters; the most is {max}", s.chars().count()));
        return None;
    }
    Some(s)
}

/// A key's value as a whole number within `min..=max`, or a problem.
fn number_of(p: &mut Problems, t: &Table, key: &str, min: i64, max: Option<i64>, what: &str) -> Option<i64> {
    let (k, item) = t.get_key_value(key)?;
    let Some(n) = item.as_integer() else {
        p.add(k.span(), format!("`{key}` must be a whole number"));
        return None;
    };
    if n < min {
        p.add(k.span(), format!("`{key}` is {n}; {what}"));
        return None;
    }
    if let Some(max) = max.filter(|m| n > *m) {
        p.add(k.span(), format!("`{key}` is {n}; the most is {max}"));
        return None;
    }
    Some(n)
}

fn unknown_keys(p: &mut Problems, t: &Table, allowed: &[&str], place: &str) {
    for (k, _) in t.iter() {
        if !allowed.contains(&k) {
            let at = t.get_key_value(k).and_then(|(key, _)| key.span());
            p.add(at, format!("unknown key `{k}` {place}; the keys are: {}", allowed.join(", ")));
        }
    }
}

/// Reads and checks `raw`. `name` is the file's name without `.toml` (`inline` for one the model wrote).
pub fn parse(name: &str, raw: &str) -> Result<Workflow, Vec<Problem>> {
    let mut p = Problems { raw, list: Vec::new() };
    if raw.len() > MAX_FILE {
        p.add(None, format!("the workflow is {} KiB; the most is {} KiB", raw.len() / 1024, MAX_FILE / 1024));
        return Err(p.list);
    }
    let doc = match Document::parse(raw) {
        Ok(d) => d,
        Err(e) => {
            p.add(e.span(), format!("not valid TOML: {}", e.message()));
            return Err(p.list);
        }
    };
    let root = doc.as_table();
    unknown_keys(&mut p, root, ROOT_KEYS, "in the workflow");
    let description =
        plain_text(&mut p, root, "description", "`description`", Some(MAX_DESCRIPTION)).unwrap_or_default();
    let max_parallel = number_of(&mut p, root, "max_parallel", 1, Some(MAX_PARALLEL as i64), "it must be at least 1")
        .map_or(DEFAULT_PARALLEL, |n| n as usize);
    let max_agents = number_of(&mut p, root, "max_agents", 1, Some(MAX_AGENTS as i64), "it must be at least 1")
        .map_or(DEFAULT_AGENTS, |n| n as usize);
    let max_tokens = number_of(&mut p, root, "max_tokens", 1, None, "0 would stop the run before it starts")
        .map_or(DEFAULT_TOKENS, |n| n as u64);

    let mut phases: Vec<Phase> = Vec::new();
    match root.get_key_value("phase") {
        None => p.add(None, "the workflow has no phase; add one with `[[phase]]`"),
        Some((k, item)) => match item.as_array_of_tables() {
            None => p.add(k.span(), "write each phase as `[[phase]]`"),
            Some(list) => {
                for t in list.iter() {
                    phases.push(read_phase(&mut p, t, phases.len() + 1));
                }
            }
        },
    }
    check_links(&mut p, &phases);

    let wf = Workflow { name: name.to_string(), description, max_parallel, max_agents, max_tokens, phases };
    let plan = wf.plan();
    if plan.certain > wf.max_agents {
        p.add(
            None,
            format!(
                "the steps without `each` start {} sub-agents at least, and max_agents is {}",
                plan.certain, wf.max_agents
            ),
        );
    }
    if p.list.is_empty() {
        Ok(wf)
    } else {
        Err(p.list)
    }
}

fn read_phase(p: &mut Problems, t: &Table, number: usize) -> Phase {
    unknown_keys(p, t, PHASE_KEYS, "in a phase");
    let title = plain_text(p, t, "title", "`title`", Some(MAX_TITLE))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Phase {number}"));
    let mut steps = Vec::new();
    match t.get_key_value("step") {
        None => p.add(t.span(), format!("phase \"{title}\" has no step; add one with `[[phase.step]]`")),
        Some((k, item)) => match item.as_array_of_tables() {
            None => p.add(k.span(), "write each step as `[[phase.step]]`"),
            Some(list) if list.is_empty() => p.add(k.span(), format!("phase \"{title}\" has no step")),
            Some(list) => {
                for s in list.iter() {
                    if let Some(step) = read_step(p, s) {
                        steps.push(step);
                    }
                }
            }
        },
    }
    Phase { title, steps }
}

fn read_step(p: &mut Problems, t: &Table) -> Option<Step> {
    unknown_keys(p, t, STEP_KEYS, "in a step");
    let line = p.line(t.span());
    let id = text_of(p, t, "id");
    match id.as_deref() {
        None if t.get("id").is_none() => p.add(t.span(), "a step has no `id`"),
        Some("") => p.add(t.get_key_value("id").and_then(|(k, _)| k.span()), "a step has an empty `id`"),
        Some(i) if !valid_id(i) => p.add(
            t.get_key_value("id").and_then(|(k, _)| k.span()),
            format!("`id` \"{}\" is not allowed; use 1 to 32 characters of a-z, 0-9, - and _", i.escape_debug()),
        ),
        _ => {}
    }
    let named = id.as_deref().map_or_else(|| "?".to_string(), |i| i.escape_debug().to_string());
    let prompt = text_of(p, t, "prompt");
    match prompt.as_deref() {
        None if t.get("prompt").is_none() => p.add(t.span(), format!("step `{named}` has no `prompt`")),
        Some(s) if s.trim().is_empty() => p.add(
            t.get_key_value("prompt").and_then(|(k, _)| k.span()),
            format!("step `{named}` has an empty `prompt`"),
        ),
        Some(s) if s.len() > MAX_PROMPT => p.add(
            t.get_key_value("prompt").and_then(|(k, _)| k.span()),
            format!("the prompt of step `{named}` is {} KiB; the most is {} KiB", s.len() / 1024, MAX_PROMPT / 1024),
        ),
        _ => {}
    }
    let list = match text_of(p, t, "returns") {
        None => false,
        Some(r) if r == "list" => true,
        Some(r) => {
            p.add(
                t.get_key_value("returns").and_then(|(k, _)| k.span()),
                format!("`returns` is \"{r}\"; only \"list\" is known"),
            );
            false
        }
    };
    let agent = plain_text(p, t, "agent", "`agent`", None).filter(|s| !s.trim().is_empty());
    let model = plain_text(p, t, "model", "`model`", None).filter(|s| !s.trim().is_empty());
    let each = text_of(p, t, "each");
    let votes = number_of(p, t, "votes", 1, Some(MAX_VOTES as i64), "it must be at least 1").map_or(1, |n| n as usize);
    let answers = read_answers(p, t, &named, votes);
    let (id, prompt) = (id?, prompt?);
    // kept even with problems of its own: the other steps are still checked against it
    Some(Step { id, agent, model, prompt, list, each, votes, answers, line })
}

/// What an answer and a report are compared as: lower case, from the first letter or digit on. The runner reads a
/// report with it, and the file is checked with it, so an entry that could never match is refused at load.
pub(super) fn answer_key(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// `answers = ["REAL", "NOT REAL"]`: 2 to 5 entries of 1 to 24 characters, only for a step with `votes` above 1.
fn read_answers(p: &mut Problems, t: &Table, named: &str, votes: usize) -> Vec<String> {
    let Some((k, item)) = t.get_key_value("answers") else { return Vec::new() };
    let at = k.span();
    let Some(list) = item.as_array() else {
        p.add(at.clone(), format!("step `{named}`: `answers` must be a list of text, like [\"REAL\", \"NOT REAL\"]"));
        return Vec::new();
    };
    let entries: Option<Vec<String>> = list.iter().map(|v| v.as_str().map(str::to_string)).collect();
    let Some(entries) = entries else {
        p.add(at.clone(), format!("step `{named}`: every entry of `answers` must be text in quotes"));
        return Vec::new();
    };
    let mut ok = true;
    if votes < 2 {
        p.add(at.clone(), format!("step `{named}`: `answers` needs `votes` above 1, since it counts the copies"));
        ok = false;
    }
    if !(MIN_ANSWERS..=MAX_ANSWERS).contains(&entries.len()) {
        p.add(
            at.clone(),
            format!("step `{named}`: `answers` has {} entries; use {MIN_ANSWERS} to {MAX_ANSWERS}", entries.len()),
        );
        ok = false;
    }
    for e in &entries {
        if e.chars().any(char::is_control) {
            p.add(at.clone(), format!("step `{named}`: an entry of `answers` has a control character"));
            ok = false;
        } else if e.is_empty() || e.chars().count() > MAX_ANSWER {
            p.add(
                at.clone(),
                format!(
                    "step `{named}`: the entry \"{e}\" of `answers` has {} characters; use 1 to {MAX_ANSWER}",
                    e.chars().count()
                ),
            );
            ok = false;
        } else if answer_key(e).is_empty() {
            p.add(
                at.clone(),
                format!("step `{named}`: the entry \"{e}\" of `answers` has no letter or digit, so no report can begin with it"),
            );
            ok = false;
        }
    }
    // case and the characters before the first letter or digit do not tell two answers apart when a report is read
    let mut keys: Vec<(String, &String)> = Vec::new();
    for e in &entries {
        let key = answer_key(e);
        if key.is_empty() {
            continue;
        }
        if let Some((_, first)) = keys.iter().find(|(k, _)| *k == key) {
            p.add(
                at.clone(),
                format!(
                    "step `{named}`: the entries \"{first}\" and \"{e}\" of `answers` read the same; keep one of them"
                ),
            );
            ok = false;
        } else {
            keys.push((key, e));
        }
    }
    if ok {
        entries
    } else {
        Vec::new()
    }
}

/// The checks that look across steps: duplicate ids, `each`, and the placeholders of every prompt.
fn check_links(p: &mut Problems, phases: &[Phase]) {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (i, s) in phases.iter().enumerate().flat_map(|(i, ph)| ph.steps.iter().map(move |s| (i, s))) {
        if seen.insert(s.id.as_str(), i).is_some() {
            p.list.push(Problem {
                line: s.line,
                text: format!("two steps are called `{}`; ids must be different", s.id),
            });
        }
    }
    for (i, phase) in phases.iter().enumerate() {
        for s in &phase.steps {
            let at = |text: String| Problem { line: s.line, text };
            if let Some(e) = &s.each {
                match phases[..i].iter().flat_map(|ph| &ph.steps).find(|o| o.id == *e) {
                    None => p.list.push(at(format!(
                        "step `{}`: `each` names `{e}`, which is not a step of an earlier phase",
                        s.id
                    ))),
                    Some(o) if !o.list => p.list.push(at(format!(
                        "step `{}`: `each` names `{e}`, which does not say `returns = \"list\"`",
                        s.id
                    ))),
                    Some(_) => {}
                }
            }
            match pieces(&s.prompt) {
                Err(why) => p.list.push(at(format!("step `{}`: {why}", s.id))),
                Ok(parts) => {
                    for part in parts {
                        match part {
                            Piece::Item if s.each.is_none() => {
                                p.list.push(at(format!("step `{}`: `{{{{item}}}}` needs `each` on the step", s.id)))
                            }
                            Piece::Report(id) => {
                                let earlier = phases[..i].iter().flat_map(|ph| &ph.steps).any(|o| o.id == id);
                                if !earlier {
                                    let why = if seen.contains_key(id) {
                                        "is not in an earlier phase"
                                    } else {
                                        "is not a step"
                                    };
                                    p.list.push(at(format!("step `{}`: `{{{{steps.{id}.report}}}}` {why}", s.id)));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// A prompt cut into text and the three placeholders there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Args,
    Item,
    Report(&'a str),
}

/// Cuts `prompt` at its `{{ }}` placeholders. Anything else in double braces is an error, so it never reaches a
/// model as text.
pub fn pieces(prompt: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut out = Vec::new();
    let mut rest = prompt;
    while let Some(i) = rest.find("{{") {
        if i > 0 {
            out.push(Piece::Text(&rest[..i]));
        }
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            return Err(
                "a `{{` is never closed; the only placeholders are {{args}}, {{item}} and {{steps.<id>.report}}".into(),
            );
        };
        let name = after[..j].trim();
        out.push(match name {
            "args" => Piece::Args,
            "item" => Piece::Item,
            n => match n.strip_prefix("steps.").and_then(|s| s.strip_suffix(".report")).filter(|s| !s.is_empty()) {
                Some(id) => Piece::Report(id),
                None => {
                    return Err(format!(
                        "unknown placeholder `{{{{{n}}}}}`; the only ones are {{{{args}}}}, {{{{item}}}} and {{{{steps.<id>.report}}}}"
                    ))
                }
            },
        });
        rest = &after[j + 2..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    Ok(out)
}

/// Fills the placeholders of `prompt` in one pass: what a placeholder brings in is never read for placeholders
/// again.
pub fn expand(prompt: &str, args: &str, item: Option<&str>, report: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(prompt.len());
    for piece in pieces(prompt).unwrap_or_default() {
        match piece {
            Piece::Text(t) => out.push_str(t),
            Piece::Args => out.push_str(args),
            Piece::Item => out.push_str(item.unwrap_or("")),
            Piece::Report(id) => out.push_str(&report(id)),
        }
    }
    out
}

/// A workflow saved as a file.
#[derive(Debug, Clone)]
pub struct Saved {
    pub name: String,
    pub path: PathBuf,
    pub text: String,
}

/// The saved workflows visible from `cwd`: the person's own, then the project's (which replace those of the
/// same name). A project nobody trusted yet (`nexo config trust`) has its workflows left out, as its agents are.
pub fn saved(cwd: &Path) -> Vec<Saved> {
    saved_from(cwd, &nexo_core::paths::config_dir(), &nexo_core::paths::trusted_projects_file())
}

pub fn saved_from(cwd: &Path, config_dir: &Path, trust_file: &Path) -> Vec<Saved> {
    let mut out: Vec<Saved> = Vec::new();
    for s in nexo_core::compat::sources(cwd, config_dir, trust_file, None, "workflows", None, None) {
        let Ok(rd) = std::fs::read_dir(&s.dir) else { continue };
        let mut files: Vec<PathBuf> =
            rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        files.sort();
        for f in files {
            let Some(name) = f.file_stem().and_then(|n| n.to_str()).filter(|n| valid_id(n)).map(str::to_string) else {
                continue;
            };
            if std::fs::metadata(&f).map_or(true, |m| m.len() as usize > MAX_FILE) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            out.retain(|o| o.name != name);
            out.push(Saved { name, path: f, text });
        }
    }
    out
}

/// The text of the lines of `Problem`s, one per line: the first `MAX_PROBLEMS`, then how many more there are.
pub fn problem_lines(list: &[Problem]) -> String {
    let mut lines: Vec<String> = list.iter().take(MAX_PROBLEMS).map(|p| format!("- {p}")).collect();
    if list.len() > MAX_PROBLEMS {
        lines.push(format!("+{} more", list.len() - MAX_PROBLEMS));
    }
    lines.join("\n")
}

/// What a saved workflow is called in a permission rule: its name and the first 16 bytes (32 hex characters) of
/// the SHA-256 of its bytes (`bugs@<32 hex>`). That is long enough that a project file cannot be padded until its
/// start equals the one in a rule. A changed file, or a project file that replaces the person's own, is another
/// subject, so a rule made for one does not cover the other; a rule with the old 8 hex matches nothing.
pub fn subject(name: &str, text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{name}@{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
description = "Find bugs in the changed files, then check each finding"
max_parallel = 3
max_agents = 20
max_tokens = 1000

[[phase]]
title = "Find"
  [[phase.step]]
  id = "find"
  agent = "explore"
  prompt = "List the bugs in {{args}}."
  returns = "list"

[[phase]]
title = "Check"
  [[phase.step]]
  id = "check"
  each = "find"
  votes = 3
  prompt = "Refute this: {{item}}. Context: {{steps.find.report}}"
"#;

    fn problems(raw: &str) -> Vec<Problem> {
        parse("t", raw).expect_err("the file should be refused")
    }

    fn says(raw: &str, want: &str) {
        let p = problems(raw);
        assert!(p.iter().any(|p| p.text.contains(want)), "wanted `{want}` in {p:?}");
    }

    /// A one-step file with `extra` lines in the step.
    fn one(extra: &str) -> String {
        format!("[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\n{extra}\n")
    }

    #[test]
    fn a_good_file_reads_back_as_written() {
        let w = parse("bugs", GOOD).unwrap();
        assert_eq!((w.max_parallel, w.max_agents, w.max_tokens), (3, 20, 1000));
        assert_eq!(w.phases.len(), 2);
        let f = &w.phases[0].steps[0];
        assert_eq!((f.id.as_str(), f.agent.as_deref(), f.list, f.votes), ("find", Some("explore"), true, 1));
        let c = &w.phases[1].steps[0];
        assert_eq!((c.each.as_deref(), c.votes, c.list), (Some("find"), 3, false));
        assert_eq!(w.plan(), Plan { certain: 1, at_most: 20 });
        let d = parse("d", &one("")).unwrap();
        assert_eq!((d.max_parallel, d.max_agents, d.max_tokens), (DEFAULT_PARALLEL, DEFAULT_AGENTS, DEFAULT_TOKENS));
    }

    #[test]
    fn every_check_refuses_the_file_and_says_where() {
        says("surprise = 1\n[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"", "unknown key `surprise`");
        says(&one("color = \"red\""), "unknown key `color`");
        says("[[phase]]\ntitle = \"A\"\nmood = 1\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"", "unknown key `mood`");
        says("[[phase]]\ntitle = \"A\"\n[[phase.step]]\nprompt = \"p\"", "has no `id`");
        says("[[phase]]\n[[phase.step]]\nid = \"\"\nprompt = \"p\"", "empty `id`");
        says(&one("").replace("id = \"a\"", "id = \"Big Id\""), "not allowed");
        says(&one("").replace("id = \"a\"", &format!("id = \"{}\"", "x".repeat(33))), "not allowed");
        says("[[phase]]\n[[phase.step]]\nid = \"a\"", "has no `prompt`");
        says(&one("").replace("prompt = \"p\"", "prompt = \"  \""), "empty `prompt`");
        says(&one("").replace("prompt = \"p\"", &format!("prompt = \"{}\"", "x".repeat(MAX_PROMPT + 1))), "KiB");
        says("description = \"x\"", "no phase");
        says("[[phase]]\ntitle = \"A\"", "has no step");
        says(&format!("max_parallel = 9\n{}", one("")), "`max_parallel` is 9; the most is 8");
        says(&format!("max_parallel = 0\n{}", one("")), "`max_parallel` is 0");
        says(&format!("max_agents = 51\n{}", one("")), "`max_agents` is 51; the most is 50");
        says(&format!("max_tokens = 0\n{}", one("")), "`max_tokens` is 0");
        says(&one("votes = 6"), "`votes` is 6; the most is 5");
        says(&one("returns = \"json\""), "only \"list\" is known");
        says(&one("each = \"zzz\""), "not a step of an earlier phase");
        says(
            "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\n[[phase.step]]\nid=\"a\"\nprompt=\"q\"",
            "two steps are called `a`",
        );
        says("this is = = not toml", "not valid TOML");
        says(&format!("# {}\n{}", "x".repeat(MAX_FILE), one("")), "the most is 64 KiB");
    }

    #[test]
    fn each_needs_an_earlier_step_that_returns_a_list() {
        let two = |first: &str, second: &str| {
            format!(
                "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\n{first}\n[[phase]]\n[[phase.step]]\nid=\"b\"\nprompt=\"q\"\n{second}"
            )
        };
        assert!(parse("t", &two("returns = \"list\"", "each = \"a\"")).is_ok());
        says(&two("", "each = \"a\""), "does not say `returns = \"list\"`");
        // the same phase is not earlier
        says(
            "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\nreturns=\"list\"\n[[phase.step]]\nid=\"b\"\nprompt=\"q\"\neach=\"a\"",
            "not a step of an earlier phase",
        );
    }

    #[test]
    fn placeholders_are_checked_at_load() {
        says(&one("").replace("\"p\"", "\"see {{nope}}\""), "unknown placeholder `{{nope}}`");
        says(&one("").replace("\"p\"", "\"see {{item}}\""), "needs `each`");
        says(&one("").replace("\"p\"", "\"see {{steps.a.report}}\""), "not in an earlier phase");
        says(&one("").replace("\"p\"", "\"see {{steps.zz.report}}\""), "is not a step");
        says(&one("").replace("\"p\"", "\"see {{args\""), "never closed");
        assert!(parse("t", &one("").replace("\"p\"", "\"see {{args}} and {{ args }}\"")).is_ok());
    }

    #[test]
    fn the_certain_steps_alone_may_not_pass_max_agents() {
        let three = "[[phase]]\n[[phase.step]]\nid=\"a\"\nprompt=\"p\"\nvotes=2\n[[phase.step]]\nid=\"b\"\nprompt=\"p\"\nvotes=2";
        says(&format!("max_agents = 3\n{three}"), "start 4 sub-agents at least");
        assert!(parse("t", &format!("max_agents = 4\n{three}")).is_ok());
    }

    #[test]
    fn all_the_problems_come_back_together_with_their_lines() {
        let raw =
            "max_parallel = 99\n\n[[phase]]\ntitle = \"A\"\n[[phase.step]]\nid = \"a\"\nprompt = \"p\"\nvotes = 9\n";
        let p = problems(raw);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(p[0].line, Some(1));
        assert_eq!(p[1].line, Some(8));
        assert!(problem_lines(&p).contains("line 8: `votes` is 9"));
    }

    #[test]
    fn an_agent_kind_the_task_tool_does_not_know_is_a_problem() {
        let w = parse("t", &one("agent = \"nobody\"")).unwrap();
        let kinds = vec!["explore".to_string(), "general".to_string()];
        let p = w.check_agents(&kinds);
        assert_eq!(p.len(), 1);
        assert!(p[0].text.contains("no sub-agent kind `nobody`") && p[0].line.is_some(), "{p:?}");
        assert!(parse("t", &one("agent = \"general\"")).unwrap().check_agents(&kinds).is_empty());
    }

    #[test]
    fn text_for_the_person_is_short_and_has_no_control_character() {
        let top = |extra: &str| format!("{extra}\n{}", one(""));
        assert!(parse("t", &top(&format!("description = \"{}\"", "d".repeat(MAX_DESCRIPTION)))).is_ok());
        says(
            &top(&format!("description = \"{}\"", "d".repeat(MAX_DESCRIPTION + 1))),
            "`description` is 201 characters",
        );
        says(&top("description = \"a\\u001b[2Jb\""), "`description` has a control character");
        let titled = |t: &str| one("").replace("title = \"A\"", &format!("title = \"{t}\""));
        assert!(parse("t", &titled(&"t".repeat(MAX_TITLE))).is_ok());
        says(&titled(&"t".repeat(MAX_TITLE + 1)), "`title` is 81 characters");
        says(&titled("a\\u001bb"), "`title` has a control character");
        says(&one("agent = \"a\\u0007b\""), "`agent` has a control character");
        says(&one("model = \"a\\u0007b\""), "`model` has a control character");
        // the id has its own rule, and the problem shows it escaped
        says(&one("").replace("id = \"a\"", "id = \"a\\u001bb\""), "not allowed");
        assert!(!problems(&one("").replace("id = \"a\"", "id = \"a\\u001bb\""))
            .iter()
            .any(|p| p.text.contains('\u{1b}')));
    }

    #[test]
    fn load_problems_are_listed_up_to_twenty_then_how_many_more() {
        let steps: String =
            (0..30).map(|n| format!("[[phase.step]]\nid = \"s{n}\"\nprompt = \"p\"\nvotes = 9\n")).collect();
        let p = problems(&format!("max_agents = 50\n[[phase]]\ntitle = \"A\"\n{steps}"));
        assert_eq!(p.len(), 30, "{p:?}");
        let lines = problem_lines(&p);
        assert_eq!(lines.lines().count(), MAX_PROBLEMS + 1, "{lines}");
        assert!(lines.ends_with("+10 more"), "{lines}");
        assert!(!problem_lines(&p[..20]).contains("more"));
    }

    #[test]
    fn answers_are_two_to_five_short_entries_and_only_with_votes() {
        let ok = |extra: &str| parse("t", &one(extra)).unwrap().phases[0].steps[0].answers.clone();
        assert_eq!(ok("votes = 3\nanswers = [\"REAL\", \"NOT REAL\"]"), ["REAL", "NOT REAL"]);
        assert!(ok("votes = 3").is_empty());
        says(&one("votes = 3\nanswers = [\"A\"]"), "has 1 entries; use 2 to 5");
        says(&one("votes = 3\nanswers = [\"a\",\"b\",\"c\",\"d\",\"e\",\"f\"]"), "has 6 entries");
        says(&one("votes = 3\nanswers = [\"a\", \"\"]"), "has 0 characters; use 1 to 24");
        says(&one(&format!("votes = 3\nanswers = [\"a\", \"{}\"]", "x".repeat(25))), "has 25 characters; use 1 to 24");
        says(&one("votes = 3\nanswers = [\"a\", \"b\\u001bc\"]"), "control character");
        says(&one("votes = 3\nanswers = [\"a\", 1]"), "must be text in quotes");
        says(&one("votes = 3\nanswers = \"a\""), "must be a list of text");
        says(&one("answers = [\"a\", \"b\"]"), "needs `votes` above 1");
        says(&one("votes = 1\nanswers = [\"a\", \"b\"]"), "needs `votes` above 1");
    }

    #[test]
    fn an_answer_with_no_letter_or_digit_and_two_that_read_the_same_are_refused_at_load() {
        // no report can begin with "???": the first letter or digit is where the comparison starts
        says(&one("votes = 3\nanswers = [\"???\", \"REAL\"]"), "no letter or digit");
        // case and leading symbols do not tell answers apart, as the count reads them
        says(&one("votes = 3\nanswers = [\"REAL\", \"real\"]"), "read the same");
        says(&one("votes = 3\nanswers = [\"- REAL\", \"Real\"]"), "read the same");
        // different words are fine, and so is one that only begins with another
        assert!(parse("t", &one("votes = 3\nanswers = [\"REAL\", \"REAL ESTATE\", \"NOT REAL\"]")).is_ok());
    }

    #[test]
    fn the_subject_of_a_saved_file_is_its_name_and_the_start_of_its_hash() {
        // SHA-256 of "abc" begins ba7816bf8f01cfea414140de5dae2223: 16 bytes, 32 hex, so padding a file to match a
        // rule is out of reach
        assert_eq!(subject("bugs", "abc"), "bugs@ba7816bf8f01cfea414140de5dae2223");
        assert_ne!(subject("bugs", "abc"), subject("bugs", "abd"));
        assert_ne!(subject("bugs", "abc"), subject("other", "abc"));
    }

    #[test]
    fn expansion_is_one_pass() {
        let report = |id: &str| format!("the report of {id} says {{{{args}}}}");
        let got = expand("A={{args}} I={{item}} R={{steps.x.report}}", "{{item}}", Some("{{args}}"), &report);
        assert_eq!(got, "A={{item}} I={{args}} R=the report of x says {{args}}");
    }

    #[test]
    fn a_projects_workflows_are_listed_only_when_it_is_trusted() {
        let proj = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(proj.path().join(".nexo/workflows")).unwrap();
        std::fs::write(proj.path().join(".nexo/workflows/mine.toml"), one("")).unwrap();
        std::fs::write(proj.path().join(".nexo/workflows/Bad Name.toml"), one("")).unwrap();
        std::fs::write(proj.path().join(".nexo/workflows/notes.md"), "x").unwrap();
        std::fs::create_dir_all(cfg.path().join("workflows")).unwrap();
        std::fs::write(cfg.path().join("workflows/mine.toml"), "# the person's own").unwrap();
        std::fs::write(cfg.path().join("workflows/own.toml"), one("")).unwrap();
        let trust = cfg.path().join("trusted-projects");
        let names = |v: Vec<Saved>| v.into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(saved_from(proj.path(), cfg.path(), &trust)), vec!["mine", "own"]);
        assert!(saved_from(proj.path(), cfg.path(), &trust)[0].text.contains("the person's own"));
        nexo_core::config::trust_project(proj.path(), &trust).unwrap();
        let all = saved_from(proj.path(), cfg.path(), &trust);
        assert_eq!(names(all.clone()), vec!["own", "mine"]);
        // the project's file replaces the person's of the same name
        assert!(all[1].text.contains("[[phase]]"));
    }
}
