//! What goes out: the output format, the startup error, notices and the event lines.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Fmt {
    Text,
    Json,
    StreamJson,
    /// `--json`: every internal agent event as it is, kept for the scripts that read it.
    Legacy,
}

/// The output format the flags ask for.
pub fn format_asked(flags: &Flags, legacy_json: bool) -> Fmt {
    match (flags.output_format, legacy_json) {
        (Some(OutputFormat::Json), _) => Fmt::Json,
        (Some(OutputFormat::StreamJson), _) => Fmt::StreamJson,
        (Some(OutputFormat::Text), _) => Fmt::Text,
        (None, true) => Fmt::Legacy,
        (None, false) => Fmt::Text,
    }
}

/// A run that could not start (no provider, a bad configuration) says so on stdout in the format asked
/// for, as the result object of docs/HEADLESS.md (the `--json` events: an `error` event).
pub fn print_startup_error(fmt: Fmt, msg: &str) {
    use std::io::Write as _;
    let line = match fmt {
        Fmt::Text => return,
        Fmt::Legacy => json!({"event": "error", "message": msg}),
        Fmt::Json | Fmt::StreamJson => {
            Run::new(fmt, Vec::new(), false, None).result(EXIT_FAILED, "", None, Some(msg.into()))
        }
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// What a failed connection adds: the address tried, and for Ollama the likely cause.
pub fn unreachable_note(provider: &str, base_url: &str) -> String {
    let ollama = provider == "ollama" || base_url.contains(":11434");
    // the address is printed without its credentials and query, as the failure card shows it
    let shown = nexo_core::problem::shown_address(base_url);
    if ollama {
        format!(" at {shown}. Ollama does not seem to be running; start it with `ollama serve`")
    } else {
        format!(" at {shown}")
    }
}

/// The text of a provider-unreachable notice with the address tried added (see `unreachable_note`); any other
/// notice is returned as it is. Shared by the `-p` run and the collected run of `nexo review`, so both say the same.
pub fn with_reach_note(reach: Option<&str>, level: Level, text: &str, card: bool) -> String {
    match (reach, text.strip_prefix("Could not reach the provider")) {
        (Some(note), Some(rest)) if level == Level::Error && !card => {
            // the note names the address; for Ollama it replaces the raw transport detail
            if note.contains("Ollama") {
                format!("Could not reach the provider{note}")
            } else {
                // the transport's words may carry the raw address; the note already names it without credentials
                format!("Could not reach the provider{note}{}", nexo_core::problem::without_url(rest))
            }
        }
        _ => text.to_string(),
    }
}

/// A notice as printed on stderr. A failure card (`card`) is its plain form, the title first and in red when
/// `color`; any other notice is `level: text`.
pub fn notice_line(level: Level, text: &str, card: bool, color: bool) -> String {
    // what a provider or a tool said never reaches the terminal with its escape sequences or control bytes
    let text = &nexo_core::problem::tidy(text);
    if !card {
        return format!("{}: {text}", format!("{level:?}").to_lowercase());
    }
    match (color, text.split_once('\n')) {
        (true, Some((title, rest))) => format!("\x1b[1;31m{title}\x1b[0m\n{rest}"),
        (true, None) => format!("\x1b[1;31m{text}\x1b[0m"),
        (false, _) => text.to_string(),
    }
}

/// Stderr is a terminal that takes colours.
pub fn color_ok() -> bool {
    let tty = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    #[cfg(windows)]
    let tty = tty && crossterm::ansi_support::supports_ansi();
    tty
}

/// The line the legacy JSON output prints for an event. The events about a sub-agent run's own conversation
/// are for the interface: they are not printed.
pub(super) fn legacy_line(ev: &AgentEvent) -> Option<String> {
    if ev.is_sub() {
        return None;
    }
    serde_json::to_string(ev).ok()
}

pub(super) fn line(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::agent::AgentEvent;
    use nexo_core::subs::{SubInfo, SubItem, SubState};
    use nexo_core::{Level, Problem, ProblemCtx, ProviderError};

    #[test]
    fn a_failure_prints_as_its_plain_card_title_first_and_red_only_on_a_terminal() {
        let e = ProviderError::from_http(401, r#"{"error":{"message":"bad key"}}"#);
        let p = Problem::of(&e, &ProblemCtx { provider: "openai".into(), ..Default::default() });
        let plain = notice_line(Level::Error, &p.plain(), true, false);
        assert_eq!(plain, p.plain());
        assert!(plain.starts_with("openai rejected the key\n"), "{plain}");
        assert!(plain.contains("1. ") && plain.contains("HTTP 401"), "{plain}");
        assert!(!plain.contains('\x1b'), "no colour off a terminal");
        let red = notice_line(Level::Error, &p.plain(), true, true);
        assert!(red.starts_with("\x1b[1;31mopenai rejected the key\x1b[0m\n"), "{red:?}");
        assert!(red.ends_with(&plain[plain.find('\n').unwrap() + 1..]), "only the title is coloured");
        // any other notice keeps its `level: text` line
        assert_eq!(notice_line(Level::Warn, "careful", false, true), "warn: careful");
    }

    #[test]
    fn a_plain_unreachable_notice_names_the_address_without_its_credentials_or_query() {
        let url = "http://user:hunter2@127.0.0.1:9/v1?key=zzz9";
        let reach = unreachable_note("local", url);
        assert_eq!(reach, " at http://127.0.0.1:9/v1");
        let t = "Could not reach the provider: error sending request for url (http://user:hunter2@127.0.0.1:9/v1?key=zzz9): connection refused";
        let out = with_reach_note(Some(&reach), Level::Error, t, false);
        assert!(
            out.starts_with("Could not reach the provider at http://127.0.0.1:9/v1: ")
                && out.ends_with("connection refused"),
            "{out}"
        );
        assert!(!out.contains("hunter2") && !out.contains("zzz9"), "{out}");
        // the Ollama form shows the clean address too
        let o = unreachable_note("ollama", "http://me:pw@localhost:11434/v1?k=zzz9");
        assert!(o.starts_with(" at http://localhost:11434/v1.") && !o.contains("pw") && !o.contains("zzz9"), "{o}");
        // a card is returned as it is
        assert_eq!(with_reach_note(Some(&reach), Level::Error, t, true), t);
    }

    #[test]
    fn the_legacy_output_prints_events_but_not_those_of_sub_agent_conversations() {
        let info = SubInfo {
            id: "a#1".into(),
            profile: "explore".into(),
            provider: None,
            model: None,
            title: "t".into(),
            state: SubState::Ok,
            started_ms: 0,
            ended_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            cost_usd: 0.0,
        };
        let subs = [
            AgentEvent::Sub { id: "a#1".into(), item: SubItem::Text { text: "x".into() } },
            AgentEvent::SubDelta { id: "a#1".into(), text: "x".into() },
            AgentEvent::SubState { info },
            AgentEvent::SubNotDelivered { id: "a#1".into(), why: "w".into(), text: "message".into() },
            AgentEvent::SubTranscript { id: "a#1".into(), request: 0, transcript: None },
            AgentEvent::FlowRun {
                info: nexo_core::subs::FlowInfo {
                    id: "wf#1".into(),
                    name: "w".into(),
                    started_ms: 0,
                    ended_ms: None,
                    state: nexo_core::subs::FlowState::Running,
                    phases: vec![],
                },
            },
        ];
        for e in &subs {
            assert_eq!(legacy_line(e), None, "{e:?}");
        }
        let line = legacy_line(&AgentEvent::TextDelta { text: "hi".into() }).unwrap();
        assert!(line.contains("\"text_delta\""), "{line}");
    }
}
