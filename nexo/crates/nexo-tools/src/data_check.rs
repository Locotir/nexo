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

/// The column a block scalar header (`key: |`, `- >-`, `- key: |2+`) hangs from, when `code` (a line
/// without its indentation or comment) ends in one. Its body is every following line indented deeper.
fn block_owner(indent: usize, code: &str) -> Option<usize> {
    let (head, token) = code.rsplit_once(char::is_whitespace).unwrap_or(("", code));
    let modifiers = token.strip_prefix(['|', '>'])?;
    if modifiers.len() > 2 || !modifiers.chars().all(|c| "+-0123456789".contains(c)) {
        return None;
    }
    // a header is a whole value: it follows `key:` or only dashes, never plain text (`a: x |`)
    let head = head.trim_end();
    if !(head.ends_with(':') || head.chars().all(|c| c == '-' || c.is_whitespace())) {
        return None;
    }
    let (mut col, mut rest, mut item) = (indent, code, None);
    while let Some(after) = rest.strip_prefix("- ") {
        item = Some(col);
        let next = after.trim_start();
        col += rest.len() - next.len();
        rest = next;
    }
    // `- |` belongs to the dash, `key: |` to the key
    Some(if rest == token { item.unwrap_or(col) } else { col })
}

/// Whether `code` (a line without its indentation or comment) ends in a plain scalar: not a key or dash
/// waiting for its value, a closed quote or flow collection, a document marker, or a tag or anchor.
fn ends_plain(code: &str) -> bool {
    let last = code.chars().next_back().unwrap_or(':');
    let token = code.rsplit(char::is_whitespace).next().unwrap_or("");
    !(":\"']}".contains(last)
        || code.chars().all(|c| c == '-' || c == '.' || c.is_whitespace())
        || token.starts_with(['&', '!']))
}

fn yaml(text: &str) -> String {
    let mut depth: i32 = 0;
    // a quoted scalar still open from an earlier line (flow scalars may span lines): its quote, the line it
    // began on and that line's indentation
    let mut open: Option<(char, usize, usize)> = None;
    // the indentation of the previous line when it ended in a plain scalar, whose deeper-indented next
    // line is a continuation of that scalar, not the start of a new node
    let mut plain: Option<usize> = None;
    // the column owning a block scalar whose body is being skipped: its text is not YAML
    let mut block: Option<usize> = None;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let body = raw.trim_start();
        let indent = raw.len() - body.len();
        if let Some(owner) = block {
            if body.is_empty() || indent > owner {
                continue;
            }
            block = None;
        }
        // a continuation line of a quoted scalar is indented deeper than the line that opened it, so a
        // line that is not ends the scalar early: the quote was never closed, whatever quote comes later
        if let Some((q, line, _)) = open.filter(|o| !body.is_empty() && indent <= o.2) {
            return format!("invalid YAML at line {line}: a {q} quote is not closed");
        }
        if open.is_none() && raw[..indent].contains('\t') && !body.is_empty() {
            return format!("invalid YAML at line {n}: a tab in the indentation");
        }
        let (mut quote, mut opened) = (open.map(|o| o.0), open.map_or((n, indent), |o| (o.1, o.2)));
        let (mut prev, mut last, mut code_end) = (' ', None::<char>, body.len());
        let mut chars = body.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            // a quote or bracket starts a flow value only where a value can start, so `a: it's` and
            // `a: x]` are plain text
            let start = match last {
                // a line that continues a plain scalar starts no node; neither does a comma outside a flow
                // collection
                None => depth > 0 || !plain.is_some_and(|p| indent > p),
                Some('[' | '{' | ',') => depth > 0,
                Some(':' | '-' | '?') => prev.is_whitespace(),
                Some(_) => false,
            };
            match (quote, c) {
                (Some('"'), '\\') => {
                    chars.next();
                }
                (Some('\''), '\'') if chars.peek().is_some_and(|p| p.1 == '\'') => {
                    chars.next();
                }
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '#') if prev.is_whitespace() => {
                    code_end = at;
                    break;
                }
                (None, '"' | '\'') if start => {
                    quote = Some(c);
                    opened = (n, indent);
                }
                (None, '[' | '{') if depth > 0 || start => depth += 1,
                (None, ']' | '}') if depth > 0 || start => depth -= 1,
                _ => {}
            }
            prev = c;
            if !c.is_whitespace() {
                last = Some(c);
            }
        }
        open = quote.map(|q| (q, opened.0, opened.1));
        // a flow collection may span lines, but it never closes before it opens
        if depth < 0 {
            return format!("invalid YAML at line {n}: a closing bracket with no opening one");
        }
        let code = body[..code_end].trim_end();
        if open.is_none() {
            block = block_owner(indent, code);
        }
        if !code.is_empty() {
            plain = (open.is_none() && depth == 0 && block.is_none() && ends_plain(code)).then_some(indent);
        }
    }
    if let Some((q, line, _)) = open {
        return format!("invalid YAML at line {line}: a {q} quote is not closed");
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
            // a lone CR ends a line too (old Mac files), so a stray one is never a record of its own
            '\n' | '\r' if !quoted => {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What `yaml` answers for a file it finds nothing wrong with, for `lines` lines.
    fn fine(lines: usize) -> String {
        format!("{} | indentation, quotes and brackets look fine (not a full parse)", nexo_core::count(lines, "line"))
    }

    #[test]
    fn a_csv_with_a_quoted_comma_in_the_header_or_a_quoted_newline_in_a_field_counts_one_record_each() {
        assert_eq!(table("\"a,b\",c\n1,2\n", false), "2 rows × 2 columns | ragged rows: 0");
        assert_eq!(table("a,b\n1,\"x\ny\"\n3,4\n", false), "3 rows × 2 columns | ragged rows: 0");
        assert_eq!(table("a,b\n1,\"say \"\"hi, you\"\"\"\n", false), "2 rows × 2 columns | ragged rows: 0");
    }

    #[test]
    fn a_csv_with_crlf_a_bom_or_a_blank_line_is_read_the_same_as_plain_lines() {
        assert_eq!(table("a,b\r\n1,2\r\n", false), "2 rows × 2 columns | ragged rows: 0");
        assert_eq!(table("\u{feff}a,b\n1,2\n", false), "2 rows × 2 columns | ragged rows: 0");
        assert_eq!(table("a,b\n\n1,2\n", false), "2 rows × 2 columns | ragged rows: 0");
    }

    #[test]
    fn a_csv_that_is_empty_or_one_column_wide_says_so() {
        assert_eq!(table("", false), "empty file");
        assert_eq!(table("\n\n", false), "empty file");
        assert_eq!(table("a\n1\n2\n", false), "3 rows × 1 column | ragged rows: 0");
    }

    #[test]
    fn ragged_csv_rows_are_named_by_the_line_they_start_on() {
        // the quoted newline pushes the last record from line 4 to line 5
        assert_eq!(
            table("a,b\n1\n\"x\ny\"\n1,2,3\n", false),
            "4 rows × 2 columns | ragged rows: 3 (line 2 has 1, line 3 has 1, line 5 has 3)"
        );
        assert_eq!(table("a,b\n1,\"x\n2,3\n", false), "a quoted field opened at line 2 is never closed");
    }

    #[test]
    fn a_lone_carriage_return_ends_a_csv_line_and_is_never_a_row_of_its_own() {
        // a stray one after the last newline used to read as a one-field row and call the table ragged
        assert_eq!(table("a,b\n1,2\n\r", false), "2 rows × 2 columns | ragged rows: 0");
        // a file from old Mac software has no other line break
        assert_eq!(table("a,b\r1,2\r", false), "2 rows × 2 columns | ragged rows: 0");
    }

    #[test]
    fn a_tsv_is_split_on_tabs_even_when_commas_are_more_common() {
        assert_eq!(table("a,b,c\td\n1,2,3\t4\n", true), "2 rows × 2 columns | ragged rows: 0");
    }

    #[test]
    fn a_report_names_the_file_and_leaves_other_extensions_alone() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.yml"), "a: 1\n").unwrap();
        std::fs::write(d.path().join("a.txt"), "a: 1\n").unwrap();
        assert_eq!(report(&d.path().join("a.yml")), format!("\ncheck: a.yml | {}", fine(1)));
        assert_eq!(report(&d.path().join("a.txt")), "");
        assert!(is_data(&d.path().join("X.YAML")) && !is_data(&d.path().join("a.txt")));
    }

    #[test]
    fn a_double_quoted_scalar_that_runs_over_several_lines_is_not_an_unclosed_quote() {
        assert_eq!(yaml("a: \"one\n  two\"\nb: 1\n"), fine(3));
        assert_eq!(yaml("a: 'one\n  two ''three'' four'\n"), fine(2));
        assert_eq!(yaml("a: \"say \\\"hi\\\"\"\n"), fine(1));
    }

    #[test]
    fn the_body_of_a_block_scalar_is_text_and_is_not_checked_for_quotes_or_brackets() {
        assert_eq!(yaml("run: |\n  echo 'unclosed\n  echo [x\nnext: 1\n"), fine(4));
        assert_eq!(yaml("run: >-\n  a \"b\n\n  c\nnext: 1\n"), fine(5));
        assert_eq!(yaml("steps:\n  - |\n    echo [x\n  - run: |2+\n      echo {y\n    name: z\n"), fine(6));
        // a tab inside the body is content, not an indentation error
        assert_eq!(yaml("run: |\n  a\n  \tb\n"), fine(3));
        // the line after the body is checked again
        assert_eq!(yaml("run: |\n  a\nnext: \"x\n"), "invalid YAML at line 3: a \" quote is not closed");
    }

    #[test]
    fn a_quote_inside_a_plain_scalar_does_not_open_a_quoted_one() {
        assert_eq!(yaml("a: the 'quoted word\nb: it's here\n"), fine(2));
        assert_eq!(yaml("a: see [the docs\nb: x]\n"), fine(2));
    }

    #[test]
    fn an_unclosed_quote_is_named_even_when_a_later_line_holds_a_quote_of_the_same_kind() {
        let at2 = "invalid YAML at line 2: a \" quote is not closed";
        assert_eq!(yaml("a: 1\nb: \"x\nc: 2\nd: \"y\"\n"), at2);
        assert_eq!(yaml("a: 1\nb: 'x\nc: 2\nd: 'y'\n"), "invalid YAML at line 2: a ' quote is not closed");
        assert_eq!(yaml("a:\n  - 'x\n  - 'y'\n"), "invalid YAML at line 2: a ' quote is not closed");
        // a continuation indented deeper than the line that opened the quote stays a multi-line scalar
        assert_eq!(yaml("a:\n  - 'x\n    y'\n  - z\n"), fine(4));
        assert_eq!(yaml("a: \"x\n\n  y\"\nb: 1\n"), fine(4));
    }

    #[test]
    fn a_quote_or_bracket_after_a_comma_in_a_plain_scalar_or_on_its_continuation_line_is_text() {
        assert_eq!(yaml("title: Hello, \"world\nb: 1\n"), fine(2));
        assert_eq!(yaml("a: one, [two\nb: 1\n"), fine(2));
        assert_eq!(yaml("k: v\n  'quoted continuation\nz: 1\n"), fine(3));
        assert_eq!(yaml("- v\n  [continuation\n- w\n"), fine(3));
        // inside a flow collection a comma still starts a value, and a quoted key still opens a quote
        assert_eq!(yaml("a: [1, \"x\n"), "invalid YAML at line 1: a \" quote is not closed");
        assert_eq!(yaml("a: 1\n\"b\": \"x\n"), "invalid YAML at line 2: a \" quote is not closed");
    }
    #[test]
    fn a_comment_may_hold_quotes_and_brackets() {
        assert_eq!(yaml("a: 1 # it's [odd\n# \"also\n"), fine(2));
    }

    #[test]
    fn crlf_multi_document_files_duplicate_keys_and_prose_are_accepted_as_a_light_check_does() {
        assert_eq!(yaml("a: 1\r\nb: 2\r\n"), fine(2));
        assert_eq!(yaml("a: 1\n---\nb: 2\n---\nc: 3\n"), fine(5));
        // a duplicate key needs a real parse; the check says it is not one
        assert_eq!(yaml("a: 1\na: 2\n"), fine(2));
        assert_eq!(yaml("This is prose.\nIt has no structure at all.\n"), fine(2));
    }

    #[test]
    fn a_tab_an_unclosed_quote_or_bracket_and_a_stray_closer_are_named_with_their_line() {
        assert_eq!(yaml("a:\n\t- b\n"), "invalid YAML at line 2: a tab in the indentation");
        assert_eq!(yaml("a: 1\nb: \"x\nc: 2\n"), "invalid YAML at line 2: a \" quote is not closed");
        assert_eq!(yaml("a: [1, 2\nb: 3\n"), "invalid YAML at line 2: a bracket is not closed");
        assert_eq!(yaml("a: 1\nb: ]\n"), "invalid YAML at line 2: a closing bracket with no opening one");
        // a flow list over several lines is fine
        assert_eq!(yaml("a: [1,\n  2]\n"), fine(2));
    }
}
