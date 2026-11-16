//! Which hints a session has had, and the round that adds them to the results.

use std::path::Path;

use nexo_core::message::{Message, Part};
use serde_json::Value;

use super::{line, Kind, ATTACH_MARK, HEAD, HEAD_FAULT, HEAD_GDB, HEAD_TRACE};
use crate::hint_cmd;

/// Which hints the session has had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Told {
    pub gdb: bool,
    pub trace: bool,
    pub fault: bool,
    pub attach: bool,
}

impl Told {
    pub(super) fn has(&self, k: Kind) -> bool {
        match k {
            Kind::Gdb => self.gdb,
            Kind::Trace => self.trace,
            Kind::Fault => self.fault,
            Kind::Attach => self.attach,
        }
    }

    pub(super) fn set(&mut self, k: Kind) {
        match k {
            Kind::Gdb => self.gdb = true,
            Kind::Trace => self.trace = true,
            Kind::Fault => self.fault = true,
            Kind::Attach => self.attach = true,
        }
    }

    pub(super) fn all(&self) -> bool {
        self.gdb && self.trace && self.fault && self.attach
    }
}

/// Which hints `messages` already hold in a tool result.
pub fn told(messages: &[Message]) -> Told {
    let mut t = Told::default();
    for m in messages {
        for p in &m.content {
            let Part::ToolResult { content, .. } = p else { continue };
            for text in content.iter().filter_map(Part::as_text) {
                t.gdb |= text.contains(HEAD_GDB) || text.contains(HEAD);
                t.trace |= text.contains(HEAD_TRACE);
                t.fault |= text.contains(HEAD_FAULT) || text.contains(HEAD);
                t.attach |= text.contains(ATTACH_MARK);
            }
        }
    }
    t
}

/// The results of one round with the hint added to the first that earns each kind; `told` is set as they are.
pub fn round(told: &mut Told, calls: &[(String, String, Value)], results: Vec<Part>, cwd: &Path) -> Vec<Part> {
    if told.all() {
        return results;
    }
    results
        .into_iter()
        .map(|p| {
            let Part::ToolResult { call_id, name, mut content, is_error } = p else { return p };
            let command = calls
                .iter()
                .find(|(id, n, _)| *id == call_id && n == "bash")
                .filter(|(_, _, a)| {
                    !nexo_tools::util::bool_arg(a, "background") && !nexo_tools::util::bool_arg(a, "detach")
                })
                .and_then(|(_, _, a)| a.get("command").and_then(Value::as_str));
            let text: String = content.iter().filter_map(Part::as_text).collect::<Vec<_>>().join("\n");
            // a command that was refused, blocked or interrupted never ran: it spends nothing
            if !told.all() && (!is_error || hint_cmd::ran(&text)) {
                if let Some((kind, l)) = command.and_then(|c| line(c, &text, cwd, told)) {
                    debug_assert!(!told.has(kind));
                    told.set(kind);
                    content = vec![Part::text(format!("{text}\n\n{l}"))];
                }
            }
            Part::ToolResult { call_id, name, content, is_error }
        })
        .collect()
}
