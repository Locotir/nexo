//! The bound: every answer is a list of rows, and a page takes some of them.

use serde_json::{json, Map, Value};

use crate::{Error, Result};

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
        self.page_ext(from, budget, json, false)
    }

    /// Like [`Output::page_as`]; `name_lines` puts `--lines` in the resume command even when it is the default
    /// (the caller's page was sized by a range, so the default would not say the same).
    fn page_ext(&self, from: Option<u64>, budget: usize, json: bool, name_lines: bool) -> Page {
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
            let lines_flag =
                if budget == DEFAULT_LINES && !name_lines { String::new() } else { format!(" --lines {budget}") };
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

/// How many lines a caller wants in a page: a number, or every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    Rows(usize),
    All,
}

impl Lines {
    /// `--lines N` or `--lines all`.
    pub fn parse(s: &str) -> Result<Lines> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("all") {
            return Ok(Lines::All);
        }
        t.parse().map(Lines::Rows).map_err(|_| Error::Usage(format!("--lines takes a number or all; got {s:?}")))
    }

    fn budget(self) -> usize {
        match self {
            Lines::Rows(n) => n.max(MIN_LINES),
            Lines::All => usize::MAX,
        }
    }
}

/// What a caller asks of an answer's page, the same for `nexo bin` and `debug`: `--lines N|all`, `--from K`,
/// `--grep RE` and `--json`. Both tools cut every answer through [`PageAsk::page`].
#[derive(Debug, Clone, Default)]
pub struct PageAsk {
    pub lines: Option<Lines>,
    pub from: Option<u64>,
    pub grep: Option<String>,
    pub json: bool,
    /// The caller asked for a range or a length: without `lines`, the page is all of it.
    pub sized: bool,
}

impl PageAsk {
    /// Lines in the page: what `lines` says, else all of a range that was asked for, else [`DEFAULT_LINES`].
    pub fn budget(&self) -> usize {
        match self.lines {
            Some(l) => l.budget(),
            None if self.sized => usize::MAX,
            None => DEFAULT_LINES,
        }
    }

    /// The `from` and budget to give an op that builds only the rows of a window. A row filter needs every row,
    /// so with `--grep` the op builds all of them and [`PageAsk::page`] cuts after the filter.
    pub fn window(&self) -> (Option<u64>, usize) {
        match self.grep {
            Some(_) => (Some(0), usize::MAX),
            None => (self.from, self.budget()),
        }
    }

    /// An error when `--grep` is not a regular expression, so a caller can refuse the call before it runs.
    pub fn check(&self) -> Result<()> {
        match &self.grep {
            Some(p) => regex::Regex::new(p).map(drop).map_err(|e| Error::Bad(format!("bad regular expression: {e}"))),
            None => Ok(()),
        }
    }

    /// The page of `out`: rows that match `--grep` (their keys stay, so `--from` keeps its meaning), starting at
    /// `--from`, at most `--lines` of them. With `--grep` one line more comes first and says how many rows matched of how many.
    pub fn page(&self, out: &Output) -> Result<Page> {
        let budget = self.budget();
        let name_lines = self.sized && matches!(self.lines, Some(Lines::Rows(n)) if n.max(MIN_LINES) == DEFAULT_LINES);
        let Some(pat) = self.grep.as_deref() else {
            return Ok(out.page_ext(self.from, budget, self.json, name_lines));
        };
        let re = regex::Regex::new(pat).map_err(|e| Error::Bad(format!("bad regular expression: {e}")))?;
        let kept: Vec<Row> = out.rows.iter().filter(|r| re.is_match(&r.text)).cloned().collect();
        let (matched, total) = (kept.len(), out.rows.len() + out.tail);
        let filtered = Output {
            op: out.op,
            meta: out.meta.clone(),
            rows: kept,
            tail: 0,
            command: format!("{} --grep {}", out.command, shell_word(pat)),
        };
        let mut page = filtered.page_ext(self.from, budget, self.json, name_lines);
        page.lines.insert(0, format!("... --grep {pat}: {matched} of {total} rows match"));
        if let Some(o) = page.value.as_object_mut() {
            o.insert("grep".into(), json!(pat));
            o.insert("matched".into(), json!(matched));
            o.insert("of".into(), json!(total));
        }
        Ok(page)
    }
}

/// `--fields a,b,c`: keep only these keys in every object of the `items` of a JSON answer. A name that no item
/// has is an error that lists the names the items do have (an answer with no object items cannot be checked, so
/// it passes unchanged).
pub fn project_items(value: &mut Value, fields: &[String]) -> Result<()> {
    let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) else {
        return Err(Error::Usage("--fields needs an answer with rows; this one has none".into()));
    };
    let mut known: Vec<String> = Vec::new();
    for it in items.iter() {
        if let Some(o) = it.as_object() {
            for k in o.keys() {
                if !known.contains(k) {
                    known.push(k.clone());
                }
            }
        }
    }
    if !known.is_empty() {
        if let Some(bad) = fields.iter().find(|f| !known.contains(f)) {
            known.sort();
            return Err(Error::Usage(format!(
                "--fields: {bad:?} is not a field of this answer; the fields are: {}",
                known.join(", ")
            )));
        }
    }
    for it in items.iter_mut() {
        if let Some(o) = it.as_object_mut() {
            o.retain(|k, _| fields.contains(k));
        }
    }
    Ok(())
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

/// The rows `start..end` of an answer of `total` lines that a page of `budget` lines starting at line
/// `from` needs. One more row than the page shows is included, so the cut line can name where to resume.
pub(crate) fn window(from: Option<u64>, budget: usize, total: usize) -> (usize, usize) {
    let start = from.map_or(0, |k| usize::try_from(k).unwrap_or(usize::MAX)).min(total);
    (start, start.saturating_add(budget.max(MIN_LINES)).min(total))
}

/// Render only the window of `items` that a page needs. Rows are keyed by line position, so the page
/// after this one resumes with `--from <position>`. The second value counts the lines after the window.
pub(crate) fn cut<T>(
    items: &[T],
    from: Option<u64>,
    budget: usize,
    mut render: impl FnMut(&T) -> Row,
) -> (Vec<Row>, usize) {
    let (start, end) = window(from, budget, items.len());
    let rows = items[start..end]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut r = render(t);
            r.key = (start + i) as u64;
            r
        })
        .collect();
    (rows, items.len() - end)
}

#[cfg(test)]
mod ask_tests {
    use super::*;

    fn out(n: usize) -> Output {
        let rows = (0..n).map(|i| Row { key: i as u64, text: format!("row {i}"), json: None }).collect();
        Output { op: "t", meta: json!({}), rows, tail: 0, command: "nexo bin t f".into() }
    }

    #[test]
    fn lines_is_a_number_or_all_and_a_range_is_sized_by_itself() {
        assert_eq!(Lines::parse("25").unwrap(), Lines::Rows(25));
        assert_eq!(Lines::parse("ALL").unwrap(), Lines::All);
        assert!(Lines::parse("lots").unwrap_err().to_string().contains("number or all"));
        let ask = |lines, sized| PageAsk { lines, from: None, grep: None, json: false, sized };
        assert_eq!(ask(None, false).budget(), DEFAULT_LINES);
        assert_eq!(ask(None, true).budget(), usize::MAX);
        assert_eq!(ask(Some(Lines::Rows(30)), true).budget(), 30);
        assert_eq!(ask(Some(Lines::All), false).budget(), usize::MAX);
    }

    #[test]
    fn fields_keep_the_named_keys_and_name_the_known_ones_on_a_mistake() {
        let mut v = json!({"op": "t", "items": [{"addr": 16, "size": 3, "name": "a"}, {"addr": 32, "size": 4}]});
        project_items(&mut v, &["addr".to_string(), "name".to_string()]).unwrap();
        assert_eq!(v["items"], json!([{"addr": 16, "name": "a"}, {"addr": 32}]));
        assert_eq!(v["op"], "t");
        let mut v = json!({"items": [{"addr": 16, "size": 3}]});
        let e = project_items(&mut v, &["adr".to_string()]).unwrap_err().to_string();
        assert!(e.contains("\"adr\"") && e.contains("addr, size"), "{e}");
        assert_eq!(v["items"], json!([{"addr": 16, "size": 3}]));
        let mut v = json!({"items": []});
        project_items(&mut v, &["anything".to_string()]).unwrap();
        assert!(project_items(&mut json!({"op": "t"}), &["a".to_string()]).is_err());
    }

    #[test]
    fn grep_filters_before_the_page_is_cut_and_keeps_the_keys_for_from() {
        let o = out(300);
        let ask = PageAsk {
            lines: Some(Lines::Rows(12)),
            from: None,
            grep: Some("^row 2".into()),
            json: false,
            sized: false,
        };
        let p = ask.page(&o).unwrap();
        // rows 2, 20-29, 200-299: 111 match
        assert_eq!(p.lines[0], "... --grep ^row 2: 111 of 300 rows match");
        // the page of 12 and the line that says what the filter matched
        assert_eq!(p.lines.len(), 13);
        assert_eq!(p.value["matched"], 111);
        let next = p.next.unwrap();
        assert!(next.contains("--grep '^row 2'") && next.contains("--from 0xc8"), "{next}");
        // continuing from a key keeps the filter
        let more = PageAsk { from: Some(200), lines: Some(Lines::All), ..ask }.page(&o).unwrap();
        assert_eq!(more.lines.len(), 1 + 100);
        assert_eq!(more.lines[1], "row 200");
    }
}
