//! `note`: what the model wantseed.as_deref() kept when older messages leave the context. One line per item,
//! beginning with `Finding:` (a fact a result settled), `Plan:` (what is left) or `Rule:` (an
//! instruction the user gave that holds for the session). The tool changes nothing: the context
//! policy that keeps notes (the curator) reads the call itself, so the lines outlive their message.
//! With `scope` `project` or `user`, its `Rule:` lines are also saved to that level's instructions file.

use async_trait::async_trait;
use nexo_core::{Tool, ToolContext, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

/// Characters of one call at most.
pub const MAX_CHARS: usize = 4000;

pub struct Note;

/// Whether `line` is a note line: an optional list marker, then `Finding:`, `Plan:` or `Rule:`.
pub fn is_note_line(line: &str) -> bool {
    let l = line.trim_start();
    // a list marker is one of - * • followed by a space (so `**Finding**` is bold, not a marker)
    let l = match l.strip_prefix(['-', '*', '•']) {
        Some(r) if r.starts_with([' ', '\t']) => r.trim_start(),
        _ => l,
    };
    let l = l.strip_prefix("**").unwrap_or(l);
    ["Finding:", "Plan:", "Rule:"]
        .iter()
        .any(|p| l.starts_with(p) || l.starts_with(&format!("{}**:", &p[..p.len() - 1])))
}

/// The words of a `Rule:` note line, or None for any other line.
pub fn rule_text(line: &str) -> Option<String> {
    if !is_note_line(line) {
        return None;
    }
    let l = line.trim_start().trim_start_matches(['-', '*', '•']).trim_start();
    let l = l.trim_start_matches("**");
    let l = l.strip_prefix("Rule:**").or_else(|| l.strip_prefix("Rule**:")).or_else(|| l.strip_prefix("Rule:"))?;
    let l = l.trim();
    (!l.is_empty()).then(|| l.to_string())
}

#[async_trait]
impl Tool for Note {
    fn name(&self) -> &str {
        "note"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "note".into(),
            description: "Keep what you learnt before older messages leave this context. Write one line per item: \
                          `Finding: ...` for a fact a result settled, `Plan: ...` for what is left (it replaces the previous plan), \
                          `Rule: ...` for an instruction the user gave that holds for the whole session. The lines \
                          are kept with the first message, in your words, for as long as the session lasts. Send it in the same response as your next tool call after a \
                          result settles something, and when the context asks for it; never as a response of \
                          its own, and never after your final result."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The note lines, each beginning with `Finding:`, `Plan:` or `Rule:`."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["session", "project", "user"],
                        "description": "Where `Rule:` lines last: this session (default), the project or the user."
                    }
                },
                "required": ["text"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn summary(&self, args: &Value) -> String {
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let short: String = first.chars().take(72).collect();
        if short.len() < first.len() {
            format!("note: {short}…")
        } else {
            format!("note: {short}")
        }
    }

    async fn run(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let text = args.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() {
            return ToolOutput::error("`text` is required: one line per note, beginning with Finding:, Plan: or Rule:");
        }
        if text.chars().count() > MAX_CHARS {
            return ToolOutput::error(format!(
                "the note is over {MAX_CHARS} characters: keep the facts, not the output they came from"
            ));
        }
        let n = text.lines().filter(|l| is_note_line(l)).count();
        if n == 0 {
            return ToolOutput::error(
                "no line begins with Finding:, Plan: or Rule:; write one line per item with one of those prefixes",
            );
        }
        let other = text.lines().filter(|l| !l.trim().is_empty() && !is_note_line(l)).count();
        let scope = args.get("scope").and_then(Value::as_str).unwrap_or("session");
        let mut saved = None;
        if scope != "session" {
            let home = nexo_core::compat::foreign_home();
            let Some((file, seed)) =
                nexo_core::compat::rules_file(scope, &ctx.cwd, &nexo_core::paths::config_dir(), home.as_deref())
            else {
                return ToolOutput::error("`scope` is session, project or user");
            };
            let mut new = 0;
            for r in text.lines().filter_map(rule_text) {
                match nexo_core::compat::save_rule(&file, seed.as_deref(), &r) {
                    Ok(true) => new += 1,
                    Ok(false) => {}
                    Err(e) => return ToolOutput::error(format!("cannot save to {}: {e}", file.display())),
                }
            }
            saved = Some((new, file));
        }
        let mut out = format!("noted {n} line{}", if n == 1 { "" } else { "s" });
        if other > 0 {
            out += &format!(" ({other} without a Finding:/Plan:/Rule: prefix left out)");
        }
        if let Some((new, file)) = saved {
            out += &format!("; {new} new rule{} saved to {}", if new == 1 { "" } else { "s" }, file.display());
        }
        ToolOutput::ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn note_lines_are_recognised_with_list_markers_and_bold() {
        assert!(is_note_line("Finding: the rates are in cents."));
        assert!(is_note_line("  - Plan: port the invoices."));
        assert!(is_note_line("* **Rule:** never touch main."));
        assert!(is_note_line("**Finding**: x = 3"));
        assert!(!is_note_line("The rates are in cents."));
        assert!(!is_note_line("finding: lower case is a word, not a mark"));
    }

    #[tokio::test]
    async fn a_project_rule_is_saved_once_and_a_session_rule_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        let cx = ToolContext::new(d.path().to_path_buf());
        let call = json!({"text": "Finding: a = 1\n- Rule: never edit vendor/.", "scope": "project"});
        let first = Note.run(call.clone(), &cx).await;
        assert!(!first.is_error && first.content.contains("1 new rule saved"), "{}", first.content);
        assert!(Note.run(call, &cx).await.content.contains("0 new rules saved"));
        let f = d.path().join("AGENTS.md");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "## Rules\n- never edit vendor/.\n");
        let local = Note.run(json!({"text": "Rule: answer briefly"}), &cx).await;
        assert_eq!(local.content, "noted 1 line");
        assert_eq!(std::fs::read_to_string(&f).unwrap().matches("briefly").count(), 0);
        assert!(Note.run(json!({"text": "Rule: x", "scope": "galaxy"}), &cx).await.is_error);
    }

    #[tokio::test]
    async fn a_note_is_counted_and_a_bare_text_is_refused() {
        let cx = ToolContext::new(PathBuf::from("."));
        let ok = Note.run(json!({"text": "Finding: a = 1\nPlan: check b\nnot a note"}), &cx).await;
        assert!(!ok.is_error);
        assert_eq!(ok.content, "noted 2 lines (1 without a Finding:/Plan:/Rule: prefix left out)");
        let one = Note.run(json!({"text": "Rule: answer in Spanish"}), &cx).await;
        assert_eq!(one.content, "noted 1 line");
        assert!(Note.run(json!({"text": "just text"}), &cx).await.is_error);
        assert!(Note.run(json!({}), &cx).await.is_error);
        assert!(Note.run(json!({"text": format!("Finding: {}", "x".repeat(MAX_CHARS))}), &cx).await.is_error);
        assert_eq!(Note.summary(&json!({"text": "\nFinding: a = 1\nPlan: b"})), "note: Finding: a = 1");
    }
}
