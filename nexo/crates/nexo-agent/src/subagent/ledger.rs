//! What a sub-agent did, read from its own tool calls and what the tools answered, not from its words:
//! the files it wrote, the commands it ran and how they ended, the writes another sub-agent held back.
//! The agent that started it gets this with the report, so "the tests pass" can be checked against it.

use super::*;

/// Files, commands and refusals listed before the rest are only counted: a long run stays small.
const SHOWN_FILES: usize = 8;
const SHOWN_COMMANDS: usize = 8;
const SHOWN_REFUSALS: usize = 3;
/// Characters of a command shown.
const COMMAND_CHARS: usize = 80;

const CLAIM_MID: &str = "` is being changed by another sub-agent running at the same time (";
const CLAIM_END: &str = "); leave it to that one and say so in your report";

/// What a sub-agent is told when a file it asked to change is held by another one at work.
pub(super) fn claim_refusal(path: &str, holder: &str) -> String {
    format!("`{path}{CLAIM_MID}{holder}{CLAIM_END}")
}

/// The path and the holder in a refusal made by [`claim_refusal`].
fn refusal_of(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('`')?;
    let (path, rest) = rest.split_once(CLAIM_MID)?;
    let (holder, _) = rest.split_once(CLAIM_END)?;
    Some((path, holder))
}

/// How a path was left, as far as its tool results say.
#[derive(Default)]
struct File {
    path: String,
    added: usize,
    removed: usize,
    /// Lines of a file `write` created, or of one it replaced.
    created: Option<usize>,
    replaced: Option<usize>,
    /// Its tool results gave no line counts.
    counted: bool,
}

impl File {
    fn says(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(n) = self.created {
            parts.push(format!("new, {}", nexo_core::count(n, "line")));
        }
        if let Some(n) = self.replaced {
            parts.push(format!("rewritten, {}", nexo_core::count(n, "line")));
        }
        if self.added + self.removed > 0 || (self.created.is_none() && self.replaced.is_none() && self.counted) {
            parts.push(format!("+{} -{}", self.added, self.removed));
        }
        parts.join(", ")
    }
}

/// What a sub-agent's tool log says it did.
#[derive(Default)]
pub(super) struct Effects {
    /// Tool calls made, of every kind.
    pub(super) calls: usize,
    files: Vec<File>,
    commands: Vec<String>,
    refused: Vec<String>,
}

impl Effects {
    pub(super) fn of(messages: &[Message]) -> Effects {
        let mut answers: std::collections::HashMap<&str, (String, bool)> = std::collections::HashMap::new();
        for p in messages.iter().flat_map(|m| &m.content) {
            if let Part::ToolResult { call_id, content, is_error, .. } = p {
                let text: Vec<&str> = content.iter().filter_map(Part::as_text).collect();
                answers.insert(call_id, (text.join("\n"), *is_error));
            }
        }
        let mut fx = Effects::default();
        for m in messages.iter().filter(|m| m.role == Role::Assistant) {
            for p in &m.content {
                if let Part::ToolCall { id, name, args } = p {
                    fx.calls += 1;
                    fx.call(name, args, answers.get(id.as_str()));
                }
            }
        }
        fx
    }

    fn call(&mut self, name: &str, args: &Value, answer: Option<&(String, bool)>) {
        match name {
            "bash" => {
                let command = args.get("command").and_then(Value::as_str).unwrap_or("");
                let first = command.lines().next().unwrap_or("");
                let mut shown: String = first.chars().take(COMMAND_CHARS).collect();
                if shown.chars().count() < first.chars().count() || command.lines().count() > 1 {
                    shown.push('…');
                }
                self.commands.push(format!("ran `{shown}`: {}", ended(answer)));
            }
            "edit" | "write" => {
                let Some((text, is_error)) = answer else { return };
                if let Some((path, label)) = refusal_of(text).filter(|_| *is_error) {
                    self.refused.push(format!("refused {path}: held by {label}"));
                    return;
                }
                let seen = self.parse(text);
                // a failed `edit` of several files still wrote the ones before the failure
                if !seen && !*is_error {
                    for p in nexo_core::tool::call_paths(args) {
                        self.file(&p);
                    }
                }
            }
            _ => {}
        }
    }

    fn file(&mut self, path: &str) -> &mut File {
        match self.files.iter().position(|f| f.path == path) {
            Some(i) => &mut self.files[i],
            None => {
                self.files.push(File { path: path.to_string(), ..File::default() });
                self.files.last_mut().expect("just pushed")
            }
        }
    }

    /// The files an `edit` or `write` result says it wrote; whether it named any.
    fn parse(&mut self, text: &str) -> bool {
        let mut any = false;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("edited ") {
                let Some((head, tail)) = rest.rsplit_once(": +") else { continue };
                let Some((added, tail)) = tail.split_once(" \u{2212}") else { continue };
                let (Ok(added), Some(Ok(removed))) =
                    (added.parse::<usize>(), tail.split_whitespace().next().map(str::parse::<usize>))
                else {
                    continue;
                };
                let f = self.file(strip_note(head));
                (f.added, f.removed, f.counted) = (f.added + added, f.removed + removed, true);
                any = true;
            } else if let Some((verb, rest)) =
                line.split_once(' ').filter(|(v, _)| matches!(*v, "created" | "replaced"))
            {
                let Some((path, tail)) = rest.split_once(": file now has ") else { continue };
                let Some(Ok(n)) = tail.split_whitespace().next().map(str::parse::<usize>) else { continue };
                let f = self.file(path);
                if verb == "created" {
                    f.created = Some(n);
                } else {
                    f.replaced = Some(n);
                }
                any = true;
            }
        }
        any
    }

    /// The facts, a line each, at most about 25 lines however long the run was.
    pub(super) fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in self.files.iter().take(SHOWN_FILES) {
            let says = f.says();
            out.push(if says.is_empty() { format!("wrote {}", f.path) } else { format!("wrote {}: {says}", f.path) });
        }
        more(&mut out, self.files.len(), SHOWN_FILES, "files");
        out.extend(self.commands.iter().take(SHOWN_COMMANDS).cloned());
        more(&mut out, self.commands.len(), SHOWN_COMMANDS, "commands");
        out.extend(self.refused.iter().take(SHOWN_REFUSALS).cloned());
        more(&mut out, self.refused.len(), SHOWN_REFUSALS, "refusals");
        out
    }

    /// The block appended to a report: the count of calls and the facts. Empty when it made no call.
    pub(super) fn block(&self) -> String {
        if self.calls == 0 {
            return String::new();
        }
        let mut out = format!("[tool log] {}", nexo_core::count(self.calls, "tool call"));
        let lines = self.lines();
        if !lines.is_empty() {
            out.push(':');
        }
        for l in lines {
            out.push_str(&format!("\n- {l}"));
        }
        out
    }
}

fn more(out: &mut Vec<String>, total: usize, shown: usize, what: &str) {
    if total > shown {
        out.push(format!("and {} more {what}", total - shown));
    }
}

/// A path as `edit` names it, without its note (" (3 changes)", " (4 occurrences)").
fn strip_note(head: &str) -> &str {
    match head.rsplit_once(" (") {
        Some((path, note)) if note.ends_with(" changes)") || note.ends_with(" occurrences)") => path,
        _ => head,
    }
}

/// How a `bash` call ended, from its result.
fn ended(answer: Option<&(String, bool)>) -> String {
    let Some((text, is_error)) = answer else { return "no result".into() };
    if text.starts_with("Started background job") {
        return "started in the background".into();
    }
    if text.contains("\n\n[killed: timed out after") {
        return "timed out".into();
    }
    if text.contains("\n\n[cancelled]") {
        return "stopped".into();
    }
    if let Some(i) = text.find("\n\n[killed by a signal") {
        // `[killed by a signal]` (a terminal that gives no number) or `[killed by a signal: 9 SIGKILL]`
        let rest = &text[i + "\n\n[killed by a signal".len()..];
        if rest.starts_with(']') {
            return "killed by a signal".into();
        }
        if let Some(which) = rest.strip_prefix(": ").and_then(|r| r.split_once(']')).map(|(w, _)| w) {
            return format!("killed by a signal: {which}");
        }
    }
    if let Some(i) = text.rfind("\n\n[exit code ") {
        let code: String = text[i + "\n\n[exit code ".len()..].chars().take_while(|c| *c != ']').collect();
        if code.parse::<i64>().is_ok() {
            return format!("exit {code}");
        }
    }
    if *is_error {
        return "could not run".into();
    }
    "exit 0".into()
}

#[cfg(test)]
mod edit_summary_tests {
    use super::Effects;

    #[test]
    fn parses_edit_summary_with_removal_note_on_following_line() {
        let mut effects = Effects::default();
        assert!(effects.parse(
            "edited a.txt: +2 −1 lines, verified\nremoved newly added invisible characters: 1 zero-width space"
        ));
        assert_eq!(effects.files[0].path, "a.txt");
        assert_eq!((effects.files[0].added, effects.files[0].removed), (2, 1));
    }
}
