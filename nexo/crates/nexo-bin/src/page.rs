//! The bound: every answer is a list of rows, and a page takes some of them.

use serde_json::{json, Map, Value};

/// Lines in an answer unless the caller says otherwise.
pub const DEFAULT_LINES: usize = 120;
/// The smallest budget. Lower values are raised to it, so a cut never lands next to the header row.
pub const MIN_LINES: usize = 10;

/// One line of an answer. `key` is what `--from` names (an address or a position) and never goes down.
#[derive(Debug, Clone)]
pub struct Row {
    pub key: u64,
    pub text: String,
    /// The same row for `--json`. Rows without one (headings) are left out of the JSON items.
    pub json: Option<Value>,
}

/// A whole answer, not yet cut.
#[derive(Debug, Clone)]
pub struct Output {
    pub op: &'static str,
    /// Facts about the answer as a whole (JSON object).
    pub meta: Value,
    pub rows: Vec<Row>,
    /// Rows after the last one in `rows` that were counted but not built (see `func_window`).
    pub tail: usize,
    /// The command that asked, without `--from` and `--lines`; the cut line adds them.
    pub command: String,
}

/// A piece of an [`Output`].
#[derive(Debug, Clone)]
pub struct Page {
    /// The text lines, the last one says what is left when the answer was cut.
    pub lines: Vec<String>,
    pub value: Value,
    pub shown: usize,
    pub left: usize,
    /// The exact command for the next piece.
    pub next: Option<String>,
}

impl Page {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

impl Output {
    /// The whole answer as text, with no cut.
    pub fn text_all(&self) -> String {
        self.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// Take at most `budget` lines (cut line included) starting at the first row whose key is `from`
    /// or more.
    pub fn page(&self, from: Option<u64>, budget: usize) -> Page {
        self.page_as(from, budget, false)
    }

    /// Like [`Output::page`]; `json` makes the resume command ask for JSON again.
    pub fn page_as(&self, from: Option<u64>, budget: usize, json: bool) -> Page {
        let budget = budget.max(MIN_LINES);
        let start = match from {
            Some(k) => self.rows.partition_point(|r| r.key < k),
            None => 0,
        };
        let avail = self.rows.len() - start + self.tail;
        let cut = avail > budget;
        let shown = if cut { budget - 1 } else { avail };
        let left = avail - shown;
        let mut lines: Vec<String> = self.rows[start..start + shown].iter().map(|r| r.text.clone()).collect();
        let next = if cut {
            let key = self.rows[start + shown].key;
            let lines_flag = if budget == DEFAULT_LINES { String::new() } else { format!(" --lines {budget}") };
            let json_flag = if json { " --json" } else { "" };
            Some(format!("{}{json_flag} --from {key:#x}{lines_flag}", self.command))
        } else {
            None
        };
        if let Some(cmd) = &next {
            lines.push(format!("... {left} more lines: {cmd}"));
        } else if avail == 0 && from.is_some() {
            lines.push(format!(
                "... nothing at or after {:#x}: the answer has {} lines",
                from.unwrap_or(0),
                self.rows.len()
            ));
        }
        let items: Vec<Value> = self.rows[start..start + shown].iter().filter_map(|r| r.json.clone()).collect();
        let mut obj: Map<String, Value> = match &self.meta {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        obj.insert("op".into(), json!(self.op));
        obj.insert("items".into(), Value::Array(items));
        obj.insert("shown".into(), json!(shown));
        obj.insert("left".into(), json!(left));
        obj.insert("next".into(), next.as_ref().map_or(Value::Null, |c| json!(c)));
        Page { lines, value: Value::Object(obj), shown, left, next }
    }
}

/// A word safe to paste into a shell: quoted only when it has to be.
pub fn shell_word(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '+' | '=' | ',' | '@' | '%')
        });
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
