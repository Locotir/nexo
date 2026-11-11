//! `note`: what the model wants kept when older messages leave the context. One line per item,
//! beginning with `Finding:` (a fact a result settled), `Plan:` (what is left) or `Rule:` (an
//! instruction the user gave that holds for the session). The tool changes nothing: the context
//! policy that keeps notes (the curator) reads the call itself, so the lines outlive their message.
//! With `scope` `global` or `project`, its `Rule:` lines are also saved for later sessions: to the
//! person's `~/.nexo/prompt.md` or to the project's instructions file, dated, under `## Remembered by nexo`.

use async_trait::async_trait;
use nexo_core::compat::Saved;
use nexo_core::{Compact, Tool, ToolContext, ToolDisplay, ToolKind, ToolOutput, ToolSpec};
use serde_json::{json, Value};

/// Characters of one call at most.
pub const MAX_CHARS: usize = 4000;

/// Characters of one saved rule at most: a line, not a paragraph.
pub const MAX_RULE: usize = 200;

/// The end of the result when a rule was saved for later sessions.
pub const SAVED_TAIL: &str =
    ". This is for later sessions and the person sees it on screen: no reply about it is needed, go on.";

/// What the model is told after the line the file already has: the choice is its own, it knows the two facts.
const NEAR_HINT: &str = "if that covers yours there is nothing to do, and if yours says something it does not, \
                         send the rule again with `anyway` true";

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
            description: "Keep what you learnt before older messages leave this context, one line per item: \
                          `Finding: ...` a fact a result settled, `Plan: ...` what is left (replaces the previous plan), \
                          `Rule: ...` an instruction the user gave. Kept with the first message, in your words, for the \
                          session. Send it in the same response as your next tool call after a result settles something, \
                          or when the context asks; never as a response of its own, never after your final result. \
                          With `scope` `global` or `project`, `Rule:` lines are also saved for later sessions \
                          (secrets refused; a near-repeat is skipped unless `anyway`)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The note lines."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["session", "project", "global"],
                        "description": "Where `Rule:` lines last: this session (default), this project (AGENTS.md) or all projects (~/.nexo/prompt.md)."
                    },
                    "anyway": { "type": "boolean" }
                },
                "required": ["text"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact> {
        Some(Compact {
            description: "Keep lines for the session; send with your next tool call, never alone.",
            params: &[
                ("text", "Lines starting `Finding:`, `Plan:` or `Rule:`"),
                ("scope", "Where `Rule:` lines last"),
                ("anyway", "Save a near-repeat too"),
            ],
        })
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
                return ToolOutput::error("`scope` is session, project or global");
            };
            let anyway = args.get("anyway").and_then(Value::as_bool).unwrap_or(false);
            let mut fresh = Vec::new();
            let mut refused = Vec::new();
            let mut near = Vec::new();
            for r in text.lines().filter_map(rule_text) {
                if let Some(why) = unfit(&r) {
                    refused.push(why);
                    continue;
                }
                match nexo_core::compat::save_rule(&file, seed.as_deref(), &r, anyway) {
                    Ok(Saved::New) => fresh.push(r.split_whitespace().collect::<Vec<_>>().join(" ")),
                    Ok(Saved::There) => {}
                    Ok(Saved::Near(line)) => near.push(line),
                    Err(e) => return ToolOutput::error(format!("cannot save to {}: {e}", file.display())),
                }
            }
            if !refused.is_empty() && fresh.is_empty() {
                return ToolOutput::error(format!("not saved: {}", refused.join("; ")));
            }
            saved = Some((fresh, refused, near, file, if scope == "project" { "project" } else { "global" }));
        }
        let mut out = format!("noted {n} line{}", if n == 1 { "" } else { "s" });
        if other > 0 {
            out += &format!(" ({other} without a Finding:/Plan:/Rule: prefix left out)");
        }
        let mut display = ToolDisplay::Plain;
        if let Some((fresh, refused, near, file, scope)) = saved {
            let n = fresh.len();
            out += &format!("; {n} new rule{} saved to {}", if n == 1 { "" } else { "s" }, file.display());
            if !refused.is_empty() {
                out += &format!(" (not saved: {})", refused.join("; "));
            }
            for line in &near {
                out += &format!(" (the file already says nearly this: \"{line}\"; {NEAR_HINT})");
            }
            if n > 0 {
                // the model goes on with the task: the card on screen already tells the person
                out += SAVED_TAIL;
                display = ToolDisplay::Remembered {
                    scope: scope.to_string(),
                    items: fresh,
                    path: file.display().to_string(),
                };
            }
        }
        ToolOutput::ok(out).with_display(display)
    }
}

/// Why `rule` must not be saved to a file every later session reads (and a project shares), if so.
fn unfit(rule: &str) -> Option<String> {
    if nexo_core::redact::redact(rule) != rule {
        return Some(
            "the rule looks like it holds a secret (a key, token or password); instruction files are read by every \
             session and may be shared, so say where the secret lives, not what it is"
                .to_string(),
        );
    }
    (rule.chars().count() > MAX_RULE).then(|| {
        format!("too long: keep the exact fact, drop the filler (a rule is one line of at most {MAX_RULE} characters)")
    })
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

    /// `NEXO_CONFIG_DIR` is one variable for the whole process: the tests that point it somewhere take turns.
    static CONFIG_DIR: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        d
    }

    #[tokio::test]
    async fn a_project_rule_is_saved_once_dated_under_its_heading_and_a_session_rule_writes_nothing() {
        let d = repo();
        let f = d.path().join("AGENTS.md");
        let mine = "# Mine\n\nNever edit vendor/ by hand.\n";
        std::fs::write(&f, mine).unwrap();
        let cx = ToolContext::new(d.path().to_path_buf());
        let call = json!({"text": "Finding: a = 1\n- Rule: deploys go through `make ship`.", "scope": "project"});
        let first = Note.run(call.clone(), &cx).await;
        assert!(!first.is_error && first.content.contains("1 new rule saved"), "{}", first.content);
        // a saved rule ends by telling the model there is nothing to answer; a rule already there does not
        assert!(first.content.ends_with(SAVED_TAIL), "{}", first.content);
        let again = Note.run(call, &cx).await;
        assert!(
            again.content.contains("0 new rules saved")
                && again.display == ToolDisplay::Plain
                && !again.content.contains(SAVED_TAIL),
            "{}",
            again.content
        );
        let text = std::fs::read_to_string(&f).unwrap();
        let (old, new) = text.split_at(mine.len());
        assert_eq!(old, mine, "the person's text is untouched");
        let date =
            regex::Regex::new(r"^\n## Remembered by nexo\n- \d{4}-\d\d-\d\d: deploys go through `make ship`\.\n$")
                .unwrap();
        assert!(date.is_match(new), "{new:?}");
        // a rule the file already says in other words is skipped
        let same = Note.run(json!({"text": "Rule: Never edit vendor/ by hand", "scope": "project"}), &cx).await;
        assert!(same.content.contains("0 new rules"), "{}", same.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text);
        let local = Note.run(json!({"text": "Rule: answer briefly"}), &cx).await;
        assert_eq!(local.content, "noted 1 line");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text);
        assert!(Note.run(json!({"text": "Rule: x", "scope": "galaxy"}), &cx).await.is_error);
    }

    #[tokio::test]
    async fn a_project_without_instructions_gets_an_agents_file_at_its_root_and_says_what_was_saved() {
        let d = repo();
        std::fs::create_dir_all(d.path().join("src/deep")).unwrap();
        let cx = ToolContext::new(d.path().join("src/deep"));
        let out = Note.run(json!({"text": "Rule: the API is on port 4100", "scope": "project"}), &cx).await;
        let f = d.path().join("AGENTS.md");
        assert!(std::fs::read_to_string(&f).unwrap().starts_with("## Remembered by nexo\n- "));
        assert!(!d.path().join("src/deep/AGENTS.md").exists());
        assert_eq!(
            out.display,
            ToolDisplay::Remembered {
                scope: "project".into(),
                items: vec!["the API is on port 4100".into()],
                path: f.display().to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_global_rule_goes_to_the_prompt_file_made_from_the_template_and_never_to_the_real_home() {
        let _turn = CONFIG_DIR.lock().await;
        let (cfg, d) = (tempfile::tempdir().unwrap(), repo());
        std::env::set_var("NEXO_CONFIG_DIR", cfg.path());
        let cx = ToolContext::new(d.path().to_path_buf());
        let call = json!({"text": "Rule: my .bashrc is at ~/dotfiles/bashrc", "scope": "global"});
        let out = Note.run(call.clone(), &cx).await;
        // a second one lands under the same heading, after the first
        let two = Note.run(json!({"text": "Rule: answer in English", "scope": "global"}), &cx).await;
        let dup = Note.run(call, &cx).await;
        std::env::remove_var("NEXO_CONFIG_DIR");
        let f = cfg.path().join("prompt.md");
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!out.is_error && !two.is_error && dup.content.contains("0 new rules"), "{}", dup.content);
        assert!(text.starts_with(nexo_core::paths::PREFS_TEMPLATE), "{text}");
        assert_eq!(text.matches("## Remembered by nexo").count(), 1, "{text}");
        let (a, b) = (text.find("my .bashrc").unwrap(), text.find("answer in English").unwrap());
        assert!(a < b, "{text}");
        assert!(!d.path().join("AGENTS.md").exists());
        assert!(matches!(&out.display, ToolDisplay::Remembered { scope, path, .. }
            if scope == "global" && path == &f.display().to_string()));
    }

    #[tokio::test]
    async fn a_rule_the_file_nearly_says_is_not_saved_again_and_the_result_shows_the_line_that_is_there() {
        let d = repo();
        let cx = ToolContext::new(d.path().to_path_buf());
        let first = json!({"text": "Rule: the staging database is on port 5433 of db-stage", "scope": "project"});
        assert!(Note.run(first, &cx).await.content.contains("1 new rule saved"));
        let f = d.path().join("AGENTS.md");
        let one = std::fs::read_to_string(&f).unwrap();
        let again = json!({"text": "Rule: staging database listens on port 5433 on host db-stage", "scope": "project"});
        let out = Note.run(again.clone(), &cx).await;
        assert!(!out.is_error && out.content.contains("0 new rules saved"), "{}", out.content);
        assert!(out.content.contains("the file already says nearly this"), "{}", out.content);
        assert!(out.content.contains(": the staging database is on port 5433 of db-stage\""), "{}", out.content);
        assert!(out.content.contains("`anyway`") && !out.content.contains(SAVED_TAIL), "{}", out.content);
        assert!(matches!(out.display, ToolDisplay::Plain), "nothing was saved, so no card");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), one);
        // the model can insist when its line says something the other does not
        let mut anyway = again;
        anyway["anyway"] = json!(true);
        let out = Note.run(anyway, &cx).await;
        assert!(out.content.contains("1 new rule saved") && out.content.ends_with(SAVED_TAIL), "{}", out.content);
        let two = std::fs::read_to_string(&f).unwrap();
        assert!(two.starts_with(&one) && two.contains("listens on port 5433"), "{two}");
    }

    #[tokio::test]
    async fn a_secret_is_refused_with_the_reason_and_nothing_is_written() {
        let d = repo();
        let cx = ToolContext::new(d.path().to_path_buf());
        // fake values, built at run time so no file holds one that looks like a key
        let key = ["s", "k-", &"a1B2".repeat(8)].concat();
        for rule in [
            format!("Rule: my OpenAI key is {key}"),
            "Rule: the db password: hunter2hunter2".to_string(),
            format!("Rule: use {}", ["gh", "p_", &"A1b2".repeat(6)].concat()),
        ] {
            let out = Note.run(json!({"text": rule, "scope": "project"}), &cx).await;
            assert!(out.is_error && out.content.contains("secret"), "{rule}: {}", out.content);
            assert!(!out.content.contains(&key));
        }
        assert!(!d.path().join("AGENTS.md").exists());
        // the others of the same call are still saved, and the refusal is said
        let mixed = Note
            .run(json!({"text": format!("Rule: tabs, not spaces\nRule: token = {key}"), "scope": "project"}), &cx)
            .await;
        assert!(
            !mixed.is_error && mixed.content.contains("1 new rule saved") && mixed.content.contains("not saved"),
            "{}",
            mixed.content
        );
        let text = std::fs::read_to_string(d.path().join("AGENTS.md")).unwrap();
        assert!(text.contains("tabs, not spaces") && !text.contains(&key), "{text}");
        // a rule is a line
        let long = Note.run(json!({"text": format!("Rule: {}", "x ".repeat(300)), "scope": "project"}), &cx).await;
        assert!(long.is_error && long.content.contains("one line"), "{}", long.content);
        assert!(long.content.contains("too long: keep the exact fact, drop the filler"), "{}", long.content);
        // the limit is 200 characters: 200 are kept, 201 are refused
        assert!(unfit(&"a".repeat(MAX_RULE)).is_none());
        assert!(unfit(&"a".repeat(MAX_RULE + 1)).is_some_and(|m| m.starts_with("too long: keep the exact fact")));
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
