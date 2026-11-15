//! The rules nexo saves for good, and the files they are saved to.

use std::path::{Path, PathBuf};

use super::*;

/// The file a lasting rule is saved to and the text to start it with when it is new: the project's
/// instructions file (`project`: the nearest one nexo reads, else `AGENTS.md` at the repository root),
/// nexo's own prompt file (`global`: `prompt.md` in `config_dir`) or the user's instructions file
/// (`user`, the older name for the same wish); `None` for any other scope.
pub fn rules_file(
    scope: &str,
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
) -> Option<(PathBuf, Option<String>)> {
    rules_file_with(scope, cwd, config_dir, home, dirs::home_dir().as_deref())
}

/// [`rules_file`] below the person's home `own_home`: a project rule asked for there is the user's.
pub(super) fn rules_file_with(
    scope: &str,
    cwd: &Path,
    config_dir: &Path,
    home: Option<&Path>,
    own_home: Option<&Path>,
) -> Option<(PathBuf, Option<String>)> {
    match scope {
        // no seed: a note is no reason to copy another agent's user file into nexo's own
        "project" if project_dirs(cwd, own_home).is_empty() => Some((user_file(config_dir, home).0, None)),
        "project" => {
            let file = project_file_with(cwd, claude_code(), own_home, config_dir);
            let file = if file.is_file() { file } else { project_root_with(cwd, own_home).join("AGENTS.md") };
            Some((file, None))
        }
        "global" => Some((config_dir.join("prompt.md"), Some(crate::paths::PREFS_TEMPLATE.to_string()))),
        "user" => Some(user_file(config_dir, home)),
        _ => None,
    }
}

/// The file a lasting rule of `scope` goes to on this machine (see `rules_file`).
pub fn rules_path(scope: &str, cwd: &Path) -> Option<PathBuf> {
    let home = foreign_home();
    rules_file(scope, cwd, &crate::paths::config_dir(), home.as_deref()).map(|(p, _)| p)
}

/// The heading the rules nexo saves go under.
pub const REMEMBERED: &str = "## Remembered by nexo";

/// What the model reads on the line under that heading, so saved rules are taken as background and not as
/// something the person just said. It goes into the text that is sent, never into the file.
pub const REMEMBERED_FRAME: &str = "(Saved by nexo in earlier sessions. Take it as background to apply when it \
                                    matters: the person did not just say it, so do not answer it or bring it up.)";

/// `text` with [`REMEMBERED_FRAME`] on the line after its `## Remembered by nexo` heading; `text` as it
/// is when it has no such heading.
pub fn framed(text: &str) -> std::borrow::Cow<'_, str> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        at += line.len();
        if line.trim().eq_ignore_ascii_case(REMEMBERED) {
            let nl = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            let gap = if line.ends_with('\n') { "" } else { nl };
            return std::borrow::Cow::Owned(format!("{}{gap}{REMEMBERED_FRAME}{nl}{}", &text[..at], &text[at..]));
        }
    }
    std::borrow::Cow::Borrowed(text)
}

/// The words of a line, lower case, without the list marker, a leading `YYYY-MM-DD:` date and the
/// punctuation: two lines with the same key say the same thing.
pub(super) fn rule_key(line: &str) -> String {
    let l = line.trim().trim_start_matches(['-', '*']).trim();
    let b = l.as_bytes();
    let dated = b.len() > 11
        && b[..10].iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
        && b[10] == b':';
    let l = if dated { &l[11..] } else { l };
    l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

/// `text` with `rule` added as a dated list line (`- 2026-11-09: the rule`) at the end of its
/// `## Remembered by nexo` section; the section is created at the end when missing. Nothing already
/// in `text` changes, byte for byte. `None` when a line with the same words is already there.
pub fn with_rule(text: &str, rule: &str, date: &str) -> Option<String> {
    let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = rule_key(&rule);
    if key.is_empty() || text.lines().any(|l| rule_key(l) == key) {
        return None;
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let item = format!("- {date}: {rule}{nl}");
    // the section: from the end of its heading line to the next heading (or the end of the text)
    let (mut at, mut start, mut end) = (0, None, text.len());
    for line in text.split_inclusive('\n') {
        if start.is_none() && line.trim().eq_ignore_ascii_case(REMEMBERED) {
            start = Some(at + line.len());
        } else if start.is_some() && line.starts_with('#') {
            end = at;
            break;
        }
        at += line.len();
    }
    let Some(start) = start else {
        let gap = match text {
            "" => "",
            t if t.ends_with('\n') => nl,
            _ => &format!("{nl}{nl}"),
        };
        return Some(format!("{text}{gap}{REMEMBERED}{nl}{item}"));
    };
    // after the last line of the section that has words, so blank lines before the next heading stay
    let body = &text[start..end];
    let used = body.trim_end().len();
    let (cut, lead) = match body[used..].find('\n') {
        _ if used == 0 => (start, if text[..start].ends_with('\n') { "" } else { nl }),
        Some(q) => (start + used + q + 1, ""),
        None => (end, nl),
    };
    Some(format!("{}{lead}{item}{}", &text[..cut], &text[cut..]))
}

/// What `save_rule` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    /// Written as a new line.
    New,
    /// A line with the same words was there: nothing written.
    There,
    /// Nothing written: this line under `## Remembered by nexo` (without its list marker) says nearly the same.
    Near(String),
}

/// Telling words a rule and a saved line need to share at least before they can count as nearly the same.
pub(super) const NEAR_WORDS: usize = 3;

/// Words that join a sentence and are in every line, so two different facts share them.
pub(super) const JOINING: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "be", "it", "its", "on", "in", "of", "to", "at", "by", "as", "for", "and",
    "or", "with", "that", "this", "if",
];

/// The words of a line that tell what it says: all but the joining ones.
pub(super) fn telling(line: &str) -> std::collections::BTreeSet<String> {
    rule_key(line).split(' ').filter(|w| !JOINING.contains(w)).map(str::to_string).collect()
}

/// The lines of the `## Remembered by nexo` section of `text` (up to the next heading), each with the byte
/// where it starts and its line end.
pub(super) fn remembered_lines(text: &str) -> Vec<(usize, &str)> {
    let (mut at, mut inside, mut out) = (0, false, Vec::new());
    for line in text.split_inclusive('\n') {
        if !inside {
            inside = line.trim().eq_ignore_ascii_case(REMEMBERED);
        } else if line.starts_with('#') {
            break;
        } else {
            out.push((at, line));
        }
        at += line.len();
    }
    out
}

/// The one of `lines` that says nearly what `rule` says (see `near_rule`).
pub(super) fn nearest<'a>(lines: &[(usize, &'a str)], rule: &str) -> Option<(usize, &'a str)> {
    let mine = telling(rule);
    let mut best: Option<(usize, (usize, &str))> = None;
    for l in lines {
        let theirs = telling(l.1);
        let shared = mine.intersection(&theirs).count();
        if shared >= NEAR_WORDS
            && shared * 10 >= mine.len().min(theirs.len()) * 6
            && best.is_none_or(|(n, _)| shared > n)
        {
            best = Some((shared, *l));
        }
    }
    best.map(|(_, l)| l)
}

/// A saved line as it is shown: without its line end and its list marker.
pub(super) fn bare(line: &str) -> String {
    line.trim().trim_start_matches(['-', '*']).trim().to_string()
}

/// The line under `## Remembered by nexo` that says nearly what `rule` says, without its list marker: at
/// least `NEAR_WORDS` telling words in common, and six in ten of those of the shorter of the two. A model saves
/// a fact again in other words once the message that showed the first save has left its context, and the
/// section grows with the same fact over and over. Only nexo's own section is compared: what the person
/// wrote elsewhere in the file is theirs, and saying it again as a rule is the caller's choice. Two different
/// facts in like words can match too, so this is a question for the caller (`save_rule` with `anyway`), not a verdict.
pub fn near_rule(text: &str, rule: &str) -> Option<String> {
    nearest(&remembered_lines(text), rule).map(|(_, l)| bare(l))
}

/// The saved line that `old` names: the list line of `lines` with its words (the marker and the date do not
/// count), else the only one that says nearly the same on both sides (`NEAR_WORDS` telling words shared, six
/// in ten of those of the longer of the two). A vague quote, or one that fits two lines, names none: the wrong
/// line rewritten is worse than none. A line the person wrote in the section without a list marker is theirs.
pub(super) fn named<'a>(lines: &[(usize, &'a str)], old: &str) -> Option<(usize, &'a str)> {
    let items = || lines.iter().copied().filter(|(_, l)| l.trim_start().starts_with(['-', '*']));
    let want = rule_key(old);
    if want.is_empty() {
        return None;
    }
    if let Some(l) = items().find(|(_, l)| rule_key(l) == want) {
        return Some(l);
    }
    let mine = telling(old);
    let mut like = items().filter(|(_, l)| {
        let theirs = telling(l);
        let shared = mine.intersection(&theirs).count();
        shared >= NEAR_WORDS && shared * 10 >= mine.len().max(theirs.len()) * 6
    });
    match (like.next(), like.next()) {
        (Some(l), None) => Some(l),
        _ => None,
    }
}

/// `text` with the line of its `## Remembered by nexo` section that `old` names (see `named`) put as `rule`,
/// dated `date`, in the same place; and that line as it was, without its list marker. `None` when the
/// section has no such line, or when a line of `text` already says `rule`. Every other byte of `text`
/// stays: the person's own lines are never the ones changed.
pub fn with_replaced(text: &str, old: &str, rule: &str, date: &str) -> Option<(String, String)> {
    let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
    let key = rule_key(&rule);
    if key.is_empty() || text.lines().any(|l| rule_key(l) == key) {
        return None;
    }
    let (at, line) = named(&remembered_lines(text), old)?;
    let end = &line[line.trim_end_matches(['\r', '\n']).len()..];
    Some((format!("{}- {date}: {rule}{end}{}", &text[..at], &text[at + line.len()..]), bare(line)))
}

/// `text` without the line of its `## Remembered by nexo` section that `old` names (see `named`), and that
/// line as it was, without its list marker. `None` when the section has no such line. Every other byte of
/// `text` stays: the person's own lines are never the ones removed.
pub fn without_line(text: &str, old: &str) -> Option<(String, String)> {
    let (at, line) = named(&remembered_lines(text), old)?;
    Some((format!("{}{}", &text[..at], &text[at + line.len()..]), bare(line)))
}

/// Takes out of `file` the saved line that `old` names (see `without_line`): `Done` with what it said, or `NoLine`.
pub fn remove_rule(file: &Path, old: &str) -> std::io::Result<Replaced> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replaced::NoLine),
        Err(e) => return Err(e),
    };
    let Some((new, was)) = without_line(&text, old) else { return Ok(Replaced::NoLine) };
    std::fs::write(file, new)?;
    Ok(Replaced::Done(was))
}

/// What `replace_rule` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replaced {
    /// The named line was rewritten; this is what it said, without its list marker.
    Done(String),
    /// Nothing written: no saved line is the one named.
    NoLine,
    /// Nothing written: a line of the file already says the rule.
    There,
}

/// Puts `rule` in the place of the saved line of `file` that `old` names (see `with_replaced`, dated today).
pub fn replace_rule(file: &Path, old: &str, rule: &str) -> std::io::Result<Replaced> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replaced::NoLine),
        Err(e) => return Err(e),
    };
    let key = rule_key(rule);
    if !key.is_empty() && text.lines().any(|l| rule_key(l) == key) {
        return Ok(Replaced::There);
    }
    let today = time::OffsetDateTime::now_utc().date();
    let Some((new, was)) = with_replaced(&text, old, rule, &today.to_string()) else { return Ok(Replaced::NoLine) };
    std::fs::write(file, new)?;
    Ok(Replaced::Done(was))
}

/// Saves `rule` into `file` (see `with_rule`, dated today), starting from `seed` when the file is new.
/// A rule the remembered section nearly says already (`near_rule`) is not written unless `anyway`.
pub fn save_rule(file: &Path, seed: Option<&str>, rule: &str, anyway: bool) -> std::io::Result<Saved> {
    let old = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => seed.unwrap_or("").to_string(),
        Err(e) => return Err(e),
    };
    let today = time::OffsetDateTime::now_utc().date();
    let Some(new) = with_rule(&old, rule, &today.to_string()) else { return Ok(Saved::There) };
    if !anyway {
        if let Some(line) = near_rule(&old, rule) {
            return Ok(Saved::Near(line));
        }
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, new)?;
    Ok(Saved::New)
}
