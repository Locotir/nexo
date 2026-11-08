//! Data files checked by their format, so the model does not spend steps counting rows with `wc` and
//! `awk` or parsing a file by hand: CSV and TSV (rows, columns, rows with another number of columns),
//! JSON and TOML (parsed, the first error with its line) and YAML (a light check: indentation with
//! tabs, an unclosed quote or bracket). One `check:` line, the shape the project checks report in.

use std::path::Path;

/// Largest file looked at; a bigger one is left alone.
const MAX_BYTES: u64 = 8 << 20;
/// Ragged rows named in the line.
const RAGGED_NAMED: usize = 3;

/// Whether `path` is a data file this module knows, by its extension.
pub fn is_data(path: &Path) -> bool {
    kind(path).is_some()
}

fn kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "csv" => "csv",
        "tsv" => "tsv",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

/// `\ncheck: name | what was found` for the data file at `path`; empty when it is not one, is too large
/// or cannot be read as text.
pub fn report(path: &Path) -> String {
    let Some(kind) = kind(path) else { return String::new() };
    if std::fs::metadata(path).map_or(true, |m| m.len() > MAX_BYTES) {
        return String::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return String::new() };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let found = match kind {
        "csv" | "tsv" => table(&text, kind == "tsv"),
        "json" => json(&text),
        "toml" => toml_file(&text),
        _ => yaml(&text),
    };
    format!("\ncheck: {name} | {found}")
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes().iter().take(offset.min(text.len())).filter(|b| **b == b'\n').count() + 1
}

fn json(text: &str) -> String {
    match serde_json::from_str::<serde::de::IgnoredAny>(text) {
        Ok(_) => "valid JSON".into(),
        Err(e) => format!("invalid JSON at line {} column {}: {}", e.line(), e.column(), short(&e.to_string())),
    }
}

fn toml_file(text: &str) -> String {
    match toml::from_str::<toml::Table>(text) {
        Ok(_) => "valid TOML".into(),
        Err(e) => {
            let line = e.span().map_or(1, |s| line_of(text, s.start));
            format!("invalid TOML at line {line}: {}", short(e.message()))
        }
    }
}

/// The message without the position the checker appends (`at line 3 column 5`).
fn short(msg: &str) -> String {
    let msg = msg.lines().next().unwrap_or("");
    let msg = msg.rsplit_once(" at line ").map_or(msg, |(m, _)| m);
    msg.chars().take(120).collect()
}

fn yaml(text: &str) -> String {
    let mut depth: i32 = 0;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let indent: String = raw.chars().take_while(|c| c.is_whitespace()).collect();
        if indent.contains('\t') && !raw.trim().is_empty() {
            return format!("invalid YAML at line {n}: a tab in the indentation");
        }
        let (mut quote, mut prev) = (None::<char>, ' ');
        for c in raw.trim_start().chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '#') if prev.is_whitespace() => break,
                (None, '"') | (None, '\'') if prev.is_whitespace() || "[{,:".contains(prev) => quote = Some(c),
                (None, '[') | (None, '{') => depth += 1,
                (None, ']') | (None, '}') => depth -= 1,
                _ => {}
            }
            prev = c;
        }
        if let Some(q) = quote {
            return format!("invalid YAML at line {n}: a {q} quote is not closed");
        }
        // a flow collection may span lines, but it never closes before it opens
        if depth < 0 {
            return format!("invalid YAML at line {n}: a closing bracket with no opening one");
        }
    }
    if depth > 0 {
        return format!("invalid YAML at line {}: a bracket is not closed", text.lines().count());
    }
    format!(
        "{} | indentation, quotes and brackets look fine (not a full parse)",
        nexo_core::count(text.lines().count(), "line")
    )
}

/// The delimiter of a table: a tab for TSV, else the one that occurs most in the header line.
fn delimiter(header: &str, tsv: bool) -> char {
    if tsv {
        return '\t';
    }
    let mut best = (',', 0usize);
    for d in [',', ';', '\t', '|'] {
        let mut quoted = false;
        let mut n = 0;
        for c in header.chars() {
            if c == '"' {
                quoted = !quoted;
            } else if c == d && !quoted {
                n += 1;
            }
        }
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

/// The records of a table as `(line it starts on, fields)`, quotes honoured (a quoted field may hold
/// the delimiter and line breaks); a blank line is not a record. The error names an unclosed quote.
fn records(text: &str, delim: char) -> Result<Vec<(usize, usize)>, usize> {
    let mut out = Vec::new();
    let (mut line, mut start) = (1usize, 1usize);
    let (mut fields, mut quoted, mut blank) = (1usize, false, true);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
            }
            '"' => {
                quoted = !quoted;
                blank = false;
            }
            '\n' if quoted => line += 1,
            '\r' if !quoted && chars.peek() == Some(&'\n') => {}
            '\n' => {
                if !blank {
                    out.push((start, fields));
                }
                line += 1;
                (start, fields, blank) = (line, 1, true);
            }
            c if c == delim && !quoted => {
                fields += 1;
                blank = false;
            }
            _ => blank = false,
        }
    }
    if quoted {
        return Err(start);
    }
    if !blank {
        out.push((start, fields));
    }
    Ok(out)
}

fn table(text: &str, tsv: bool) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delim = delimiter(text.lines().next().unwrap_or(""), tsv);
    let rows = match records(text, delim) {
        Ok(r) => r,
        Err(at) => return format!("a quoted field opened at line {at} is never closed"),
    };
    let Some(&(_, columns)) = rows.first() else { return "empty file".into() };
    let ragged: Vec<&(usize, usize)> = rows.iter().filter(|r| r.1 != columns).collect();
    let mut s = format!("{} × {}", nexo_core::count(rows.len(), "row"), nexo_core::count(columns, "column"));
    s.push_str(&format!(" | ragged rows: {}", ragged.len()));
    if !ragged.is_empty() {
        let named: Vec<String> = ragged.iter().take(RAGGED_NAMED).map(|(l, n)| format!("line {l} has {n}")).collect();
        let more = ragged.len().saturating_sub(RAGGED_NAMED);
        let more = if more > 0 { format!(", +{more} more") } else { String::new() };
        s.push_str(&format!(" ({}{more})", named.join(", ")));
    }
    s
}
