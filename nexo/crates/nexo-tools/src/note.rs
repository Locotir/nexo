//! `note`: what the model wants kept when older messages leave the context. One line per item,
//! beginning with `Finding:` (a fact a result settled), `Plan:` (what is left) or `Rule:` (an
//! instruction the user gave that holds for the session). The tool changes nothing: the context
//! policy that keeps notes (the curator) reads the call itself, so the lines outlive their message.
//! With `scope` `global` or `project`, its `Rule:` lines are also saved for later sessions: to the
//! person's `~/.nexo/prompt.md` or to the project's instructions file, dated, under `## Remembered by nexo`.

use async_trait::async_trait;
use nexo_core::compat::{Replaced, Saved};
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
const NEAR_HINT: &str = "if that covers yours there is nothing to do; if yours corrects it, send the rule again with \
                         `replaces` set to that line; if it is another fact, with `anyway` true";

/// Characters of a line of the file shown back in a result at most.
const SHOWN: usize = 240;

/// A line of the person's file as a result may show it: no secret in it, and not longer than `SHOWN`.
fn shown(line: &str) -> String {
    let safe = nexo_core::redact::redact(line);
    match safe.char_indices().nth(SHOWN) {
        Some((at, _)) => format!("{}…", &safe[..at]),
        None => safe.into_owned(),
    }
}

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
                          (a near-repeat is skipped unless `anyway`; `replaces`: the saved line a rule corrects)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "scope": {
                        "type": "string",
                        "enum": ["session", "project", "global"],
                        "description": "Where `Rule:` lines last: this session (default), this project (AGENTS.md) or all projects (~/.nexo/prompt.md)."
                    },
                    "anyway": { "type": "boolean" },
                    "replaces": { "type": "string" }
                },
                "required": ["text"]
            }),
        }
    }

    fn compact(&self) -> Option<Compact<'_>> {
        Some(Compact {
            description: "Keep lines for the session; send with your next tool call, never alone.",
            params: &[
                ("text", "Lines starting `Finding:`, `Plan:` or `Rule:`"),
                ("scope", "Where `Rule:` lines last"),
                ("anyway", "Save a near-repeat too"),
                ("replaces", "The saved line this rule corrects"),
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
        let replaces = args.get("replaces").and_then(Value::as_str).map(str::trim).filter(|o| !o.is_empty());
        if replaces.is_some() && (scope == "session" || text.lines().filter_map(rule_text).count() != 1) {
            return ToolOutput::error(
                "`replaces` corrects one saved line: send exactly one `Rule:` line, with `scope` `project` or `global`",
            );
        }
        let (mut replaced, mut unmatched, mut already) = (None, false, false);
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
                if let Some(old) = replaces {
                    match nexo_core::compat::replace_rule(&file, old, &r) {
                        Ok(Replaced::Done(was)) => {
                            fresh.push(r.split_whitespace().collect::<Vec<_>>().join(" "));
                            replaced = Some(was);
                            continue;
                        }
                        // the file has the right rule already: the named line is not nexo's to take out
                        Ok(Replaced::There) => {
                            already = true;
                            continue;
                        }
                        // no line is named: the rule is saved as any other, and the result says so
                        Ok(Replaced::NoLine) => unmatched = true,
                        Err(e) => return ToolOutput::error(format!("cannot save to {}: {e}", file.display())),
                    }
                }
                match nexo_core::compat::save_rule(&file, seed.as_deref(), &r, anyway) {
                    Ok(Saved::New) => fresh.push(r.split_whitespace().collect::<Vec<_>>().join(" ")),
                    Ok(Saved::There) => {}
                    Ok(Saved::Near(line)) => near.push(line),
                    Err(e) => return ToolOutput::error(format!("cannot save to {}: {e}", file.display())),
                }
            }
            if !refused.is_empty() && fresh.is_empty() && near.is_empty() {
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
            match &replaced {
                Some(was) => {
                    out += &format!("; 1 saved rule corrected in {} (it said: \"{}\")", file.display(), shown(was))
                }
                None => out += &format!("; {n} new rule{} saved to {}", if n == 1 { "" } else { "s" }, file.display()),
            }
            if unmatched {
                out += " (no saved line is what `replaces` says, so nothing was replaced)";
            }
            if already {
                out += " (the file already has this rule, so the line `replaces` names was left as it is)";
            }
            if !refused.is_empty() {
                out += &format!(" (not saved: {})", refused.join("; "));
            }
            for line in &near {
                out += &format!(" (the file already says nearly this: \"{}\"; {NEAR_HINT})", shown(line));
            }
            if n > 0 {
                // the model goes on with the task: the card on screen already tells the person. With a
                // near-repeat left to settle it has a choice to make first, so it is not told to go on.
                if near.is_empty() {
                    out += SAVED_TAIL;
                }
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
    async fn a_saved_rule_is_corrected_in_place_with_replaces_and_the_result_says_what_it_said() {
        let d = repo();
        let cx = ToolContext::new(d.path().to_path_buf());
        let first = json!({"text": "Rule: the staging database is on port 5433 of db-stage\nRule: never touch vendor/", "scope": "project"});
        assert!(Note.run(first, &cx).await.content.contains("2 new rules saved"));
        let f = d.path().join("AGENTS.md");
        let before = std::fs::read_to_string(&f).unwrap();
        // a correction in like words is a near-repeat: the answer names `replaces`, and nothing is written
        let fix = json!({"text": "Rule: the staging database is on port 5434 of db-stage", "scope": "project"});
        let out = Note.run(fix.clone(), &cx).await;
        assert!(out.content.contains("0 new rules saved") && out.content.contains("`replaces`"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
        // with the old line named, that line is rewritten where it was and the other one stays
        let mut with = fix.clone();
        with["replaces"] = json!("the staging database is on port 5433 of db-stage");
        let out = Note.run(with.clone(), &cx).await;
        assert!(!out.is_error && out.content.contains("1 saved rule corrected in"), "{}", out.content);
        assert!(out.content.contains(": the staging database is on port 5433 of db-stage\")"), "{}", out.content);
        assert!(out.content.ends_with(SAVED_TAIL) && !out.content.contains("new rule"), "{}", out.content);
        assert!(
            matches!(&out.display, ToolDisplay::Remembered { items, .. } if items == &["the staging database is on port 5434 of db-stage"]),
            "{:?}",
            out.display
        );
        let after = std::fs::read_to_string(&f).unwrap();
        assert_eq!(after, before.replace("port 5433", "port 5434"));
        assert_eq!(after.lines().filter(|l| l.starts_with("- ")).count(), 2, "{after}");
        // the right rule is in the file already: said, and the line named stays
        with["replaces"] = json!("never touch vendor/");
        let out = Note.run(with.clone(), &cx).await;
        assert!(
            out.content.contains("0 new rules saved") && out.content.contains("already has this rule"),
            "{}",
            out.content
        );
        assert_eq!(std::fs::read_to_string(&f).unwrap(), after);
        // a line that is not there: said, and the rule is saved as any other
        let lost = json!({"text": "Rule: backups run at 03:00 UTC", "scope": "project", "replaces": "the relay keeps events for seven days"});
        let out = Note.run(lost, &cx).await;
        assert!(
            out.content.contains("1 new rule saved") && out.content.contains("nothing was replaced"),
            "{}",
            out.content
        );
        let last = std::fs::read_to_string(&f).unwrap();
        assert!(last.starts_with(&after) && last.ends_with(": backups run at 03:00 UTC\n"), "{last}");
        assert_eq!(last.lines().filter(|l| l.starts_with("- ")).count(), 3, "{last}");
        // one rule at a time, and only for a saved scope; a secret is refused as ever
        for bad in [
            json!({"text": "Rule: a\nRule: b", "scope": "project", "replaces": "never touch vendor/"}),
            json!({"text": "Rule: never touch vendor or third_party", "replaces": "never touch vendor/"}),
            json!({"text": "Finding: x", "scope": "project", "replaces": "never touch vendor/"}),
        ] {
            let out = Note.run(bad, &cx).await;
            assert!(out.is_error && out.content.contains("exactly one `Rule:` line"), "{}", out.content);
        }
        let secret = json!({"text": "Rule: the deploy token is ghp_0123456789abcdefghijklmnopqrstuvwxyzAB", "scope": "project", "replaces": "never touch vendor/"});
        let out = Note.run(secret, &cx).await;
        assert!(out.is_error && out.content.contains("secret"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), last);
    }

    #[tokio::test]
    async fn a_near_repeat_is_still_shown_when_another_rule_of_the_call_is_refused_and_a_long_or_secret_line_is_not_echoed_whole(
    ) {
        let d = repo();
        let cx = ToolContext::new(d.path().to_path_buf());
        let long = format!("the staging database is on port 5433 of db-stage {}", "and so on ".repeat(40));
        let f = d.path().join("AGENTS.md");
        std::fs::write(&f, format!("## Remembered by nexo\n- 2026-11-01: {long}\n- 2026-11-01: deploy key ghp_0123456789abcdefghijklmnopqrstuvwxyzAB opens the staging gate\n")).unwrap();
        let both = json!({"text": format!("Rule: staging database listens on port 5433 on host db-stage\nRule: {}", "x".repeat(MAX_RULE + 1)), "scope": "project"});
        let out = Note.run(both, &cx).await;
        assert!(
            !out.is_error && out.content.contains("nearly this") && out.content.contains("not saved: too long"),
            "{}",
            out.content
        );
        assert!(out.content.contains('…') && !out.content.contains(&long), "{}", out.content);
        let key = json!({"text": "Rule: the deploy key opens the staging gate and the prod gate", "scope": "project"});
        let out = Note.run(key, &cx).await;
        assert!(out.content.contains("nearly this") && !out.content.contains("ghp_0123456789"), "{}", out.content);
        // a new rule saved beside a near-repeat: the card is there, and the model is not told to go on yet
        let two = json!({"text": "Rule: staging database listens on port 5433 on host db-stage\nRule: never touch vendor/", "scope": "project"});
        let out = Note.run(two, &cx).await;
        assert!(out.content.contains("1 new rule saved") && !out.content.contains(SAVED_TAIL), "{}", out.content);
        assert!(matches!(out.display, ToolDisplay::Remembered { .. }));
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
