//! Which browser the `browser` tool drives: nexo's own headless Chrome (the default, started on first use) or the
//! person's own logged-in browser through the nexo extension. `browsers` lists them, `select_browser` picks one by id
//! and `switch_browser` asks every running browser at once. Nothing of the person's browser is controlled until they
//! click Allow in it, so these actions can wait for that click.

use std::path::Path;
use std::time::Duration;

use nexo_browser::{AnyBrowser, BrowserEntry, LaunchOptions};
use nexo_core::ToolOutput;

use super::Action;

/// The id of the browser nexo launches itself.
const HEADLESS: &str = "headless";
/// A little over what the extension (110 s) and the host (125 s) wait for the person: a call never hangs longer.
const ASK: Duration = Duration::from_secs(150);

/// What these actions do, or `None` for any other action.
pub(super) async fn run(action: &Action<'_>, guard: &mut Option<AnyBrowser>, cwd: &Path) -> Option<ToolOutput> {
    match action {
        Action::Browsers => {
            let (found, bad) = AnyBrowser::available().await;
            Some(ToolOutput::ok(list(&found, &bad, guard.as_ref())))
        }
        Action::SelectBrowser(id) => Some(select(id, guard, cwd).await),
        Action::SwitchBrowser => Some(switch(guard, cwd).await),
        _ => None,
    }
}

/// A browser of the person's that went away (closed, or Cancel on Chrome's debugging bar) is dropped here and
/// reported once, so the next action does not quietly start another browser the model did not ask for.
pub(super) async fn lost(guard: &mut Option<AnyBrowser>) -> Option<ToolOutput> {
    match guard.as_ref() {
        Some(b) if b.is_real() && !b.is_alive().await => {}
        _ => return None,
    }
    let b = guard.take()?;
    let name = b.describe();
    let _ = b.close().await;
    Some(ToolOutput::error(format!(
        "browser: the connection to {name} ended (the browser was closed, or the person stopped nexo with Cancel on \
         Chrome's \"started debugging\" bar). Nothing is controlled now. `select_browser` or `switch_browser` asks again; \
         any other action starts a headless browser instead."
    )))
}

/// What `close` says for each kind of browser.
pub(super) fn closed(real: bool) -> &'static str {
    if real {
        "released the person's browser (the tabs nexo opened stay open for them)"
    } else {
        "browser closed"
    }
}

/// How this agent shows in the prompt the person sees: `nexo in <folder>`.
fn agent_name(cwd: &Path) -> String {
    let dir = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let name = if dir.is_empty() { "nexo".to_string() } else { format!("nexo in {dir}") };
    name.chars().filter(|c| !c.is_control()).take(40).collect()
}

/// The browsers, one per line: `id  what it is`, a star on the one in use.
fn list(found: &[BrowserEntry], bad: &[(String, String)], current: Option<&AnyBrowser>) -> String {
    let mark = |id: &str| if current.is_some_and(|b| b.id() == id) { "* " } else { "  " };
    let mut lines = vec![format!(
        "{}{HEADLESS}  nexo's own Chrome, throwaway profile, no logins (the default: starts on the first page action)",
        mark(HEADLESS)
    )];
    for b in found {
        let busy = if b.in_use.is_empty() { String::new() } else { format!(", in use by {}", b.in_use.join(", ")) };
        lines.push(format!(
            "{}{}  {} on {}: the person's own browser, logged in{busy}",
            mark(&b.id),
            b.id,
            b.name,
            b.os
        ));
    }
    for (id, why) in bad {
        lines.push(format!("  {id}  not answering: {why}"));
    }
    if found.is_empty() && bad.is_empty() {
        lines.push(format!(
            "(no browser with the nexo extension is running. The person runs `nexo chrome-setup` once; then in the browser \
             on chrome://extensions they turn on Developer mode, pick \"Load unpacked\" and select the folder {})",
            nexo_core::paths::data_dir().join("chrome-extension").display()
        ));
    }
    lines.push(
        "select_browser{id} uses one (the person clicks Allow in it); switch_browser asks every browser at once."
            .into(),
    );
    lines.join("\n")
}

/// What the model is told when it starts using the person's browser: the one moment it needs these rules, so they
/// are not in the tool text every request carries.
fn joined(b: &AnyBrowser) -> String {
    joined_text(&b.describe())
}

fn joined_text(describe: &str) -> String {
    format!(
        "using {describe}\nIt opened a tab of its own in a \"nexo\" group. `tabs` lists only the tabs nexo opened; the \
         person's other tabs are neither visible nor touched. This is their logged-in browser: never type their \
         passwords or payment details, stop and tell them. Cancel on Chrome's \"started debugging\" bar ends this \
         session only (`select_browser` asks again). `close` releases it and leaves the tabs open."
    )
}

/// Puts `new` in use and lets go of the one before it (after the new one is in, so a refusal keeps the old one).
async fn replace(guard: &mut Option<AnyBrowser>, new: AnyBrowser) {
    if let Some(old) = guard.replace(new) {
        let _ = old.close().await;
    }
}

/// Connects, waiting for the person to answer in the browser.
async fn ask(id: Option<&str>, cwd: &Path) -> Result<AnyBrowser, String> {
    match tokio::time::timeout(ASK, AnyBrowser::connect_user(id, &agent_name(cwd))).await {
        Ok(Ok(b)) => Ok(b),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("nobody answered in the browser within two minutes; nothing was controlled".into()),
    }
}

async fn select(id: &str, guard: &mut Option<AnyBrowser>, cwd: &Path) -> ToolOutput {
    if let Some(b) = guard.as_ref().filter(|b| b.id() == id) {
        if b.is_alive().await {
            return ToolOutput::ok(format!("already using {}", b.describe()));
        }
    }
    if id == HEADLESS {
        return match AnyBrowser::launch(LaunchOptions::default()).await {
            Ok(b) => {
                let text = format!("using {}", b.describe());
                replace(guard, b).await;
                ToolOutput::ok(text)
            }
            Err(e) => ToolOutput::error(format!("browser select_browser: could not start: {e}")),
        };
    }
    match ask(Some(id), cwd).await {
        Ok(b) => {
            let text = joined(&b);
            replace(guard, b).await;
            ToolOutput::ok(text)
        }
        Err(e) => ToolOutput::error(format!("browser select_browser: {e}")),
    }
}

async fn switch(guard: &mut Option<AnyBrowser>, cwd: &Path) -> ToolOutput {
    match ask(None, cwd).await {
        Ok(b) => {
            let text = joined(&b);
            replace(guard, b).await;
            ToolOutput::ok(text)
        }
        Err(e) => ToolOutput::error(format!("browser switch_browser: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, busy: &[&str]) -> BrowserEntry {
        BrowserEntry {
            id: id.into(),
            name: "Google Chrome".into(),
            os: "Linux".into(),
            version: "1".into(),
            in_use: busy.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_list_names_each_browser_and_who_uses_it() {
        let text = list(
            &[entry("chrome-1", &[]), entry("chrome-2", &["nexo in app"])],
            &[("old".into(), "timed out".into())],
            None,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("  headless  nexo's own Chrome"), "{text}");
        assert_eq!(lines[1], "  chrome-1  Google Chrome on Linux: the person's own browser, logged in");
        assert!(lines[2].ends_with("logged in, in use by nexo in app"), "{text}");
        assert_eq!(lines[3], "  old  not answering: timed out");
        assert!(lines[4].starts_with("select_browser{id}"), "{text}");
    }

    #[test]
    fn joining_the_persons_browser_says_what_the_model_must_know() {
        let text = joined_text("your own Chrome");
        for want in
            ["using your own Chrome", "lists only the tabs nexo opened", "passwords or payment", "Cancel", "`close`"]
        {
            assert!(text.contains(want), "{want} missing from {text}");
        }
    }

    #[test]
    fn an_empty_list_says_how_to_get_a_browser() {
        let text = list(&[], &[], None);
        assert!(text.contains("nexo chrome-setup") && text.contains("headless"), "{text}");
        for want in ["Developer mode", "Load unpacked", "chrome-extension"] {
            assert!(text.contains(want), "{want} missing from {text}");
        }
    }

    #[test]
    fn the_prompt_name_is_short_and_plain() {
        assert_eq!(agent_name(Path::new("/home/a/my-app")), "nexo in my-app");
        assert_eq!(agent_name(Path::new("/")), "nexo");
        let long = agent_name(Path::new(&format!("/x/{}", "d".repeat(100))));
        assert_eq!(long.chars().count(), 40);
        assert!(!agent_name(Path::new("/x/a\nb")).contains('\n'));
    }

    #[tokio::test]
    async fn nothing_is_lost_when_there_is_no_browser() {
        assert!(lost(&mut None).await.is_none());
    }

    #[test]
    fn closing_says_what_happened_to_the_tabs() {
        assert_eq!(closed(false), "browser closed");
        assert!(closed(true).contains("stay open"));
    }
}
