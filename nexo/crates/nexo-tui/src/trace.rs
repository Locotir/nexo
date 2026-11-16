//! `NEXO_TRACE=<file>`: one line per terminal event, command and agent event, to find why a key or a
//! message went nowhere. Off unless the variable names a file. Kinds, key codes, modifiers and event
//! names only: never what was typed, never a message, never a key of a provider.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Trace {
    file: std::fs::File,
    start: Instant,
}

fn sink() -> Option<&'static Mutex<Trace>> {
    static SINK: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("NEXO_TRACE").filter(|p| !p.is_empty())?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok()?;
        let unix_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let _ = writeln!(file, "     0ms start unix_ms={unix_ms}");
        Some(Mutex::new(Trace { file, start: Instant::now() }))
    })
    .as_ref()
}

/// Whether the trace is on (so callers can skip building a line).
pub fn on() -> bool {
    sink().is_some()
}

/// One line, stamped with the milliseconds since the trace started.
pub fn note(line: &str) {
    let Some(t) = sink() else { return };
    if let Ok(mut t) = t.lock() {
        let ms = t.start.elapsed().as_millis();
        let _ = writeln!(t.file, "{ms:6}ms {line}");
    }
}

/// Time spent in one kind of work, summed over the run: calls, microseconds.
static TOTALS: Mutex<Vec<(&'static str, u64, u128)>> = Mutex::new(Vec::new());

/// Spans this long or longer get a line of their own.
const SLOW_MS: u128 = 20;

/// Times a piece of work until it is dropped; None when the trace is off, so a span costs nothing then.
pub struct Span {
    what: &'static str,
    since: Instant,
}

/// Starts timing `what` (a phase of the interface: history, layout, draw, index).
pub fn span(what: &'static str) -> Option<Span> {
    on().then(|| Span { what, since: Instant::now() })
}

impl Drop for Span {
    fn drop(&mut self) {
        let us = self.since.elapsed().as_micros();
        if let Ok(mut t) = TOTALS.lock() {
            match t.iter_mut().find(|e| e.0 == self.what) {
                Some(e) => {
                    e.1 += 1;
                    e.2 += us;
                }
                None => t.push((self.what, 1, us)),
            }
        }
        if us / 1000 >= SLOW_MS {
            note(&format!("slow {} {}ms", self.what, us / 1000));
        }
    }
}

/// One line per kind of work: how often it ran and how long it took in all.
pub fn totals(when: &str) {
    if !on() {
        return;
    }
    let rows: Vec<String> = TOTALS
        .lock()
        .map(|t| t.iter().map(|(w, n, us)| format!("{w}={}ms/{n}", us / 1000)).collect())
        .unwrap_or_default();
    note(&format!("totals at {when}: {}", rows.join(" ")));
}

/// The name of a value of an enum: the first word of its debug text (no fields, so no content).
pub fn name_of(v: &dyn std::fmt::Debug) -> String {
    let d = format!("{v:?}");
    d.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
}

/// A terminal event as the reader got it: key kind, code and modifiers, never the character.
pub fn event(e: &crossterm::event::Event) {
    if !on() {
        return;
    }
    use crossterm::event::{Event, KeyCode};
    match e {
        Event::Key(k) => {
            let code = match k.code {
                KeyCode::Char(_) => "Char".to_string(),
                KeyCode::F(n) => format!("F{n}"),
                other => name_of(&other),
            };
            note(&format!("key kind={:?} code={code} mods={:?} state={:?}", k.kind, k.modifiers, k.state));
        }
        Event::Paste(s) => note(&format!("paste chars={}", s.chars().count())),
        Event::Mouse(_) => {}
        other => note(&format!("event {}", name_of(other))),
    }
}

/// What the interface sends to the agent.
pub fn command(c: &dyn std::fmt::Debug) {
    if on() {
        note(&format!("command {}", name_of(c)));
    }
}

/// What the agent tells the interface.
pub fn agent_event(e: &dyn std::fmt::Debug) {
    if on() {
        note(&format!("agent {}", name_of(e)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_leave_out_the_fields() {
        #[derive(Debug)]
        #[allow(dead_code)]
        enum E {
            Submit { text: String },
            Plain,
        }
        assert_eq!(name_of(&E::Submit { text: "secret words".into() }), "Submit");
        assert_eq!(name_of(&E::Plain), "Plain");
    }
}
