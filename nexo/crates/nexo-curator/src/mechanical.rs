//! Deterministic, model-free shrinking of tool output text (`squeeze`).

use std::borrow::Cow;
use std::sync::LazyLock;

use crate::py::{self, Re, I};

pub const MARK: &str = "[curator]";

static MASKS: LazyLock<Vec<(Re, &'static str)>> = LazyLock::new(|| {
    vec![
        (Re::new(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", I), "<uuid>"),
        (Re::new(r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?\b", 0), "<ts>"),
        (Re::new(r"\b\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\b", 0), "<time>"),
        (Re::new(r"\b0x[0-9a-f]+\b", I), "<hex>"),
        (Re::new(r"\b(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,}\b", I), "<hex>"),
        (Re::new(r"\d+(?:\.\d+)?", 0), "0"),
    ]
});
static PATH_LINE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^(\.{0,2}/)?[\w.@+-]+(/[\w.@+-]+)+/?$|^[\w.@+-]+\.[\w]+$", 0));
static GREP_LINE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^((?:\.{0,2}/)?[\w.@+-]+(?:/[\w.@+-]+)*\.[\w]+):(\d+)([:-])(.*)$", 0));
static DECIMAL: LazyLock<Re> = LazyLock::new(|| Re::new(r"\d+(?:\.\d+)?", 0));
/// A part of a line that changes from one line of a log to the next: a word with a digit in it (a counter, a
/// number, a hash, a name with a number).
static VARIABLE: LazyLock<Re> = LazyLock::new(|| Re::new(r"\w*\d\w*", 0));
static NUMLIKE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[\d:.,%+\-/\[\]()]+$", 0));
/// "  12\t", "12→", "12: " prefixes of file viewers
pub static LINENO: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*\d+(?:\t|→|:\s|\s\|\s)", 0));

const MAX_PERIOD: usize = 8;
const IDENTICAL_RUN: usize = 4;
const SIMILAR_RUN: usize = 6;
const SIMILAR_HEAD: usize = 2;
const SIMILAR_TAIL: usize = 1;
const BLOCK_REPEATS: usize = 3;
const MAX_LINE_CHARS: usize = 2000;
const LINE_HEAD: usize = 600;
const LINE_TAIL: usize = 200;
const MIN_SAVING: usize = 200;
const SHAPE_RUN: usize = 200;
const SHAPE_HEAD: usize = 60;
const SHAPE_TAIL: usize = 20;
/// Binary data: a run of one character this long goes as its count; a stretch this long (`BIN_SPAN`) with
/// fewer than `BIN_GAP` text characters between its binary ones goes as its length (lossy only).
const BIN_RUN: usize = 16;
const BIN_SPAN: usize = 32;
const BIN_GAP: usize = 4;
/// The words of its first copy a pointer to a repeated line quotes.
const QUOTE: usize = 40;

/// The line with its variable parts masked.
fn template(line: &str) -> String {
    let mut t = py::strip(line).to_string();
    for (pat, rep) in MASKS.iter() {
        if let Cow::Owned(s) = pat.sub(&t, rep) {
            t = s;
        }
    }
    t
}

/// The decimal numbers of a line, once its identifiers (uuids, timestamps, hex) are masked.
fn numbers(line: &str) -> Vec<f64> {
    let mut t = py::strip(line).to_string();
    for (pat, rep) in MASKS.iter().take(MASKS.len() - 1) {
        if let Cow::Owned(s) = pat.sub(&t, rep) {
            t = s;
        }
    }
    DECIMAL.find_all(&t).iter().filter_map(|&(a, b)| t[a..b].parse().ok()).collect()
}

/// Whether the `r` repeats of the `period`-line block at `i` carry measurements: a number that is neither the
/// same in every repeat nor a counter (the same step each time). Lines that differ only in a counter, a
/// timestamp or an address are a log and fold; lines whose numbers wander are a table, and the numbers are
/// what the model asked for.
fn is_data(lines: &[String], i: usize, period: usize, r: usize) -> bool {
    for k in 0..period {
        let rows: Vec<Vec<f64>> = (0..r).map(|q| numbers(&lines[i + k + q * period])).collect();
        let width = rows[0].len();
        if rows.iter().any(|row| row.len() != width) {
            return true;
        }
        let wanders = |f: usize| {
            let step = rows[1][f] - rows[0][f];
            (2..r).any(|q| (rows[q][f] - rows[q - 1][f] - step).abs() > 1e-9)
        };
        if (0..width).any(wanders) {
            return true;
        }
    }
    false
}

/// Whether the `r` repeats of the `period`-line block at `i` are the same lines, not only the same form.
fn same_blocks(lines: &[String], i: usize, period: usize, r: usize) -> bool {
    (1..r).all(|q| lines[i + q * period..i + (q + 1) * period] == lines[i..i + period])
}

/// The text as a terminal shows it once the process is done: colours and other escape sequences go, and
/// what a carriage return, a backspace or a cursor move redrew (a progress bar, a spinner, a status block
/// drawn again in place) keeps only its last frame. As on a terminal, new text writes over the old one
/// character by character: what a shorter frame does not cover stays, unless a sequence erased it.
fn clean(text: &str) -> Cow<'_, str> {
    const EDGE: usize = 500;
    if !text.contains(['\r', '\x1b', '\x08']) {
        return Cow::Borrowed(text);
    }
    let mut rows: Vec<Vec<char>> = vec![Vec::new()];
    let (mut row, mut col) = (0usize, 0usize);
    let mut cs = text.chars().peekable();
    while let Some(c) = cs.next() {
        match c {
            '\n' => {
                row += 1;
                col = 0;
                if rows.len() <= row {
                    rows.push(Vec::new());
                }
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            '\x1b' => match cs.peek().copied() {
                Some('[') => {
                    cs.next();
                    // parameters and intermediates, then one final character; anything else (a line break,
                    // text past a cut-off sequence) ends the sequence and is read as itself
                    let mut arg = String::new();
                    let mut end = None;
                    while let Some(&d) = cs.peek() {
                        if !(' '..='~').contains(&d) {
                            break;
                        }
                        cs.next();
                        if ('@'..='~').contains(&d) {
                            end = Some(d);
                            break;
                        }
                        arg.push(d);
                    }
                    // a move stops at a screen edge, as on a terminal: never past `EDGE` rows or columns
                    // (or past the text already there), whatever the number asks
                    let n = arg.split(';').next().and_then(|x| x.parse::<usize>().ok()).map(|n| n.min(EDGE));
                    match end {
                        Some('A') => row = row.saturating_sub(n.unwrap_or(1).max(1)),
                        Some('B') => {
                            row = (row + n.unwrap_or(1).max(1)).min(row.max(rows.len() - 1).max(EDGE));
                            while rows.len() <= row {
                                rows.push(Vec::new());
                            }
                        }
                        Some('C') => col = (col + n.unwrap_or(1).max(1)).min(col.max(rows[row].len()).max(EDGE)),
                        Some('D') => col = col.saturating_sub(n.unwrap_or(1).max(1)),
                        Some('G') => col = n.unwrap_or(1).max(1) - 1,
                        Some('K') => {
                            let line = &mut rows[row];
                            match n.unwrap_or(0) {
                                0 => line.truncate(col),
                                1 => line.iter_mut().take(col + 1).for_each(|x| *x = ' '),
                                _ => line.clear(),
                            }
                        }
                        _ => {}
                    }
                }
                // a two-character sequence (a character set) takes one more
                Some(d) if (' '..='/').contains(&d) => {
                    cs.next();
                    cs.next_if(|&e| e != '\n');
                }
                // an operating system command (a window title, a link) runs to a bell or a string terminator;
                // one cut off ends at the line break, which is read as itself
                Some(']') => {
                    cs.next();
                    while let Some(d) = cs.next_if(|&d| d != '\n') {
                        if d == '\x07' || (d == '\x1b' && cs.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // any other sequence is one more character (`ESC 7`, `ESC M`); a lone escape is dropped
                Some(d) if (' '..='~').contains(&d) => {
                    cs.next();
                }
                _ => {}
            },
            _ => {
                let line = &mut rows[row];
                if col < line.len() {
                    line[col] = c;
                } else {
                    line.resize(col, ' ');
                    line.push(c);
                }
                col += 1;
            }
        }
    }
    let lines: Vec<String> = rows.into_iter().map(|r| r.into_iter().collect()).collect();
    Cow::Owned(lines.join("\n"))
}

/// A character no text is made of: a control character other than a tab, or the replacement character a
/// decoder puts where bytes were not text.
fn is_binary(c: char) -> bool {
    c == '\u{FFFD}' || (c.is_control() && c != '\t')
}

/// The line with its binary data shortened. Always: a run of one such character, as its count (nothing is
/// lost). With `lossy`: a stretch where such characters come with fewer than `BIN_GAP` other characters
/// between them, as its length; the words of text in it (`BIN_GAP` characters or more) stay, as `strings`
/// keeps them.
fn binary(line: &str, lossy: bool) -> Cow<'_, str> {
    if !line.chars().any(is_binary) {
        return Cow::Borrowed(line);
    }
    let cs: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < cs.len() {
        if !is_binary(cs[i]) {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        let (mut end, mut count) = (i + 1, 1);
        if lossy {
            let mut k = i + 1;
            while k < cs.len() && k - end < BIN_GAP {
                if is_binary(cs[k]) {
                    end = k + 1;
                    count += 1;
                }
                k += 1;
            }
            if end - i >= BIN_SPAN && count >= BIN_RUN {
                out.push_str(&format!("{MARK}({} chars of binary data)", end - i));
                i = end;
                continue;
            }
            end = i + 1;
        }
        while end < cs.len() && cs[end] == cs[i] {
            end += 1;
        }
        if end - i >= BIN_RUN {
            out.push_str(&format!("{MARK}({}×U+{:04X})", end - i, cs[i] as u32));
        } else {
            out.extend(&cs[i..end]);
        }
        i = end;
    }
    Cow::Owned(out)
}

fn long_line(line: &str) -> Cow<'_, str> {
    let n = py::len(line);
    if n <= MAX_LINE_CHARS {
        return Cow::Borrowed(line);
    }
    let cut = n - LINE_HEAD - LINE_TAIL;
    Cow::Owned(format!("{} … {MARK} +{cut} chars … {}", py::head(line, LINE_HEAD), py::tail(line, LINE_TAIL)))
}

/// Which lines of a run stand out: in a column (the words counted from the start of the line, and from its
/// end) where most lines of the run say the same word, a line that says another one (`FAILED` among `ok`,
/// `ERROR` among `INFO`, `exit 1` among `exit 0`) carries news, and no fold may hide it. A column whose words
/// all differ (a counter, a name, a time) says nothing either way.
fn odd_lines(lines: &[String]) -> Vec<bool> {
    const FROM_START: usize = 8;
    const FROM_END: usize = 2;
    let n = lines.len();
    let cols: Vec<[&str; FROM_START + FROM_END]> = lines
        .iter()
        .map(|l| {
            let w: Vec<&str> = py::split_ws(l).collect();
            let mut c = [""; FROM_START + FROM_END];
            for (k, x) in w.iter().take(FROM_START).enumerate() {
                c[k] = x;
            }
            for (k, x) in w.iter().rev().take(FROM_END).enumerate() {
                c[FROM_START + k] = x;
            }
            c
        })
        .collect();
    let mut odd = vec![false; n];
    for k in 0..FROM_START + FROM_END {
        let mut count: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for c in &cols {
            *count.entry(c[k]).or_insert(0) += 1;
        }
        let Some((&top, &most)) = count.iter().max_by_key(|(_, &c)| c) else { continue };
        if 2 * most > n && most < n {
            for (j, c) in cols.iter().enumerate() {
                if c[k] != top {
                    odd[j] = true;
                }
            }
        }
    }
    odd
}

/// The fold that hides most at line `i`: (period, repeats), or (0, 0). Without `lossy` only blocks that are
/// the same, character for character, fold (their count brings every one back); single lines of one form
/// still do, as a table of what differs (`table`). A line that stands out never joins a fold here: a word
/// that differs changes its template, a number that differs makes the run data (`is_data`).
fn best_run(lines: &[String], tmpl: &[String], i: usize, lossy: bool) -> (usize, usize) {
    let mut best = (0, 0);
    let mut best_hidden = 0;
    let n = lines.len();
    for period in 1..=MAX_PERIOD {
        if i + 2 * period > n {
            break;
        }
        let block = &tmpl[i..i + period];
        if period > 1 && block.iter().all(|b| *b == block[0]) {
            continue;
        }
        let mut r = 1;
        while i + (r + 1) * period <= n && tmpl[i + r * period..i + (r + 1) * period] == *block {
            r += 1;
        }
        let hidden = if period == 1 {
            let identical = same_blocks(lines, i, 1, r);
            if identical && r >= IDENTICAL_RUN {
                r - 1
            } else if r >= SIMILAR_RUN && !is_data(lines, i, 1, r) {
                r - SIMILAR_HEAD - SIMILAR_TAIL
            } else {
                continue;
            }
        } else {
            if r < BLOCK_REPEATS || is_data(lines, i, period, r) || (!lossy && !same_blocks(lines, i, period, r)) {
                continue;
            }
            // blocks that only share their form show the last one too, as similar lines do
            if same_blocks(lines, i, period, r) {
                (r - 1) * period
            } else {
                (r - 2) * period
            }
        };
        if hidden > best_hidden {
            best = (period, r);
            best_hidden = hidden;
        }
    }
    best
}

/// Lines of one form that differ only in their counters, names with digits and hashes (a log, a progress
/// report), as one line that gives the form, with `{}` where they differ, and below it one row per line with
/// what goes in each `{}`, in order: every line comes back from it, and the parts all of them share are said
/// once. The longest run from the first line whose form is the same, when it is `SIMILAR_RUN` lines or more
/// and saves a third of them: (lines taken, lines to send).
fn table(lines: &[String]) -> Option<(usize, Vec<String>)> {
    fn split(l: &str) -> (String, Vec<&str>) {
        let (mut form, mut vals, mut at) = (String::new(), Vec::new(), 0);
        for (a, b) in VARIABLE.find_all(l) {
            form.push_str(&l[at..a]);
            form.push_str("{}");
            vals.push(&l[a..b]);
            at = b;
        }
        form.push_str(&l[at..]);
        (form, vals)
    }
    let (form, first) = split(&lines[0]);
    if first.is_empty() || form.matches("{}").count() != first.len() {
        return None;
    }
    let mut rows = vec![first.join(" ")];
    for l in &lines[1..] {
        let (f, v) = split(l);
        if f != form {
            break;
        }
        rows.push(v.join(" "));
    }
    let k = rows.len();
    let head = format!("{MARK} {k} lines of the form «{form}»; each row below holds what fills its {{}}, in order:");
    let whole: usize = lines[..k].iter().map(|l| py::len(l) + 1).sum();
    let short: usize = py::len(&head) + rows.iter().map(|r| py::len(r) + 1).sum::<usize>();
    if k < SIMILAR_RUN || 3 * short > 2 * whole {
        return None;
    }
    rows.insert(0, head);
    Some((k, rows))
}

fn collapse(lines: Vec<String>, lossy: bool) -> Vec<String> {
    let tmpl: Vec<String> = lines.iter().map(|l| template(l)).collect();
    let mut out = Vec::with_capacity(lines.len());
    let n = lines.len();
    let mut i = 0;
    while i < n {
        if tmpl[i].is_empty() {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let (period, r) = best_run(&lines, &tmpl, i, lossy);
        if period == 0 {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let end = i + period * r;
        if period == 1 {
            if same_blocks(&lines, i, 1, r) {
                out.push(lines[i].clone());
                out.push(format!("{MARK} … (×{} more identical lines)", r - 1));
            } else if !lossy {
                match table(&lines[i..end]) {
                    Some((k, t)) => {
                        out.extend(t);
                        i += k;
                    }
                    None => {
                        out.push(lines[i].clone());
                        i += 1;
                    }
                }
                continue;
            } else {
                out.extend(lines[i..i + SIMILAR_HEAD].iter().cloned());
                out.push(format!("{MARK} … (+{} more lines with the same pattern)", r - SIMILAR_HEAD - SIMILAR_TAIL));
                out.extend(lines[end - SIMILAR_TAIL..end].iter().cloned());
            }
        } else if same_blocks(&lines, i, period, r) {
            out.extend(lines[i..i + period].iter().cloned());
            out.push(format!("{MARK} … (the {period}-line block above repeats {} more times)", r - 1));
        } else {
            out.extend(lines[i..i + period].iter().cloned());
            out.push(format!("{MARK} … (+{} more {period}-line blocks with the same pattern)", r - 2));
            out.extend(lines[end - period..end].iter().cloned());
        }
        i = end;
    }
    out
}

fn group_paths(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let line = &lines[i];
        let trimmed = line.trim_end_matches('/');
        if !trimmed.contains('/') || !PATH_LINE.is_match(line) {
            out.push(line.clone());
            i += 1;
            continue;
        }
        let d = format!("{}/", trimmed.rsplit_once('/').map_or(trimmed, |(a, _)| a));
        let mut j = i;
        let mut names: Vec<&str> = Vec::new();
        while j < n
            && lines[j].starts_with(&d)
            && !lines[j][d.len()..].trim_end_matches('/').contains('/')
            && PATH_LINE.is_match(&lines[j])
        {
            names.push(&lines[j][d.len()..]);
            j += 1;
        }
        if names.len() >= 3 {
            out.push(format!("{d}{{{}}}", names.join(", ")));
            i = j;
        } else {
            out.push(line.clone());
            i += 1;
        }
    }
    out
}

fn group_grep(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let n = lines.len();
    let mut i = 0;
    let span = |l: &str, c: &py::Caps, g: usize| c[g].map_or(String::new(), |(a, b)| l[a..b].to_string());
    while i < n {
        let Some(m) = GREP_LINE.search(&lines[i]) else {
            out.push(lines[i].clone());
            i += 1;
            continue;
        };
        let f = span(&lines[i], &m, 1);
        let mut j = i;
        let mut group = Vec::new();
        while j < n {
            let Some(mj) = GREP_LINE.search(&lines[j]) else { break };
            if span(&lines[j], &mj, 1) != f {
                break;
            }
            group.push(format!("  {}{} {}", span(&lines[j], &mj, 2), span(&lines[j], &mj, 3), span(&lines[j], &mj, 4)));
            j += 1;
        }
        if group.len() >= 2 {
            out.push(format!("{f}:"));
            out.extend(group);
            i = j;
        } else {
            out.push(lines[i].clone());
            i += 1;
        }
    }
    out
}

fn shape(line: &str) -> String {
    let mut out = String::new();
    for t in py::split_ws(line).take(4) {
        if !out.is_empty() {
            out.push(' ');
        }
        let c = if py::first(t).is_some_and(py::is_digit) || NUMLIKE.is_match(t) {
            '0'
        } else if t.contains('/') {
            'P'
        } else {
            'w'
        };
        out.push(c);
    }
    out
}

/// A very long run of lines of one form keeps its head and its tail; of the middle only the lines that
/// stand out (`odd_lines`: the one `FAILED` among hundreds of `ok`) stay, in place, as many as the head
/// holds (past that it is a second kind of line more than news, and the rest of it folds).
fn clip_shape_runs(lines: Vec<String>) -> Vec<String> {
    let shapes: Vec<String> = lines.iter().map(|l| shape(l)).collect();
    let mut out = Vec::with_capacity(lines.len());
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let sig = &shapes[i];
        let mut j = i + 1;
        while j < n && !sig.is_empty() && shapes[j] == *sig {
            j += 1;
        }
        let r = j - i;
        if !sig.is_empty() && r > SHAPE_RUN {
            let odd = odd_lines(&lines[i..j]);
            out.extend(lines[i..i + SHAPE_HEAD].iter().cloned());
            let mut hidden = 0;
            let mut kept = 0;
            for k in i + SHAPE_HEAD..j - SHAPE_TAIL {
                if odd[k - i] && kept < SHAPE_HEAD {
                    kept += 1;
                    if hidden > 0 {
                        out.push(format!("{MARK} … (+{hidden} more lines of the same form)"));
                    }
                    hidden = 0;
                    out.push(lines[k].clone());
                } else {
                    hidden += 1;
                }
            }
            if hidden > 0 {
                out.push(format!("{MARK} … (+{hidden} more lines of the same form)"));
            }
            out.extend(lines[j - SHAPE_TAIL..j].iter().cloned());
        } else {
            out.extend(lines[i..j].iter().cloned());
        }
        i = j;
    }
    out
}

/// A line, or a block of lines, that the same output already sent further up, apart from this place (a
/// warning printed for each file, a stack trace logged at each retry): it goes as one line that says where
/// the first copy is, by the words that copy starts with. Exact: the copy stays above, whole. Only a run
/// that is at least twice as long as the line that stands for it goes.
fn repeats(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut first: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let l = &lines[i];
        let plain = !l.trim().is_empty() && !l.contains(MARK);
        if let Some(&j) = first.get(l.as_str()).filter(|_| plain) {
            let mut k = 0;
            while i + k < n && j + k < out.len() && out[j + k] == lines[i + k] && !lines[i + k].contains(MARK) {
                k += 1;
            }
            while k > 1 && lines[i + k - 1].trim().is_empty() {
                k -= 1;
            }
            let start = l.trim_start();
            let q = py::head(start, QUOTE);
            let cut = if q.len() < start.len() { "…" } else { "" };
            let pointer = if k == 1 {
                format!("{MARK} … (again the first line above that starts «{q}{cut}»)")
            } else {
                format!("{MARK} … (again the {k} lines above from the first one that starts «{q}{cut}»)")
            };
            let run: usize = lines[i..i + k].iter().map(|x| py::len(x) + 1).sum();
            if run >= 2 * py::len(&pointer) && !out[..j].iter().any(|x| x.trim_start().starts_with(q)) {
                out.push(pointer);
                i += k;
                continue;
            }
        }
        if plain {
            first.entry(l.as_str()).or_insert(out.len());
        }
        out.push(l.clone());
        i += 1;
    }
    out
}

/// A shrunk version of `text` (or `text` itself when the saving is negligible). With `lossy` (when the
/// whole output can be had back) lines that only share a form fold too, binary data and long lines are cut
/// short; without it only folds that lose nothing are made: repeats of the same lines go as their count or
/// as a pointer to their first copy, and what a terminal would not show (escape sequences, redrawn frames).
pub fn squeeze(text: &str, lossy: bool) -> Cow<'_, str> {
    let n = py::len(text);
    if text.is_empty() || n < MIN_SAVING {
        return Cow::Borrowed(text);
    }
    let cleaned = clean(text);
    let lines: Vec<String> = cleaned
        .split('\n')
        .map(|l| {
            let l = binary(l, lossy);
            if lossy {
                long_line(&l).into_owned()
            } else {
                l.into_owned()
            }
        })
        .collect();
    let lines = group_grep(lines);
    let lines = group_paths(lines);
    let mut lines = collapse(lines, lossy);
    if lossy {
        lines = clip_shape_runs(lines);
    }
    let lines = repeats(lines);
    let result = lines.join("\n");
    if n >= py::len(&result) + MIN_SAVING {
        Cow::Owned(result)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_runs() {
        let text = "ok\n".repeat(100);
        let s = squeeze(&text, true);
        assert!(s.contains("more identical lines"), "{s}");
        let paths: String = (0..40).map(|i| format!("src/dir/file{i}.rs\n")).collect();
        assert!(squeeze(&paths, true).starts_with("src/dir/{file0.rs, file1.rs"));
    }

    #[test]
    fn a_table_of_measurements_is_never_folded() {
        // a sweep the model ran: the numbers are the answer
        let loot = [180, 183, 187, 190, 194, 198, 201, 205, 208, 212, 216, 218, 221, 223, 226, 230, 233, 235, 240];
        let table: String = loot.iter().enumerate().map(|(l, c)| format!("luck={l} -> Loot: {c} coins\n")).collect();
        let pad = "x".repeat(MIN_SAVING);
        let text = format!("{pad}\n{table}");
        assert_eq!(*squeeze(&text, true), text);
        // the same lines with a plain counter are a log and fold
        let log: String = (0..30).map(|i| format!("[12:00:{i:02}] worker 3 handled request {i} in 40 ms\n")).collect();
        assert!(squeeze(&format!("{pad}\n{log}"), true).contains("same pattern"));
        // hexdump rows fold: an address is a counter and the words are masked
        let dump: String =
            (0..30).map(|i| format!("{:08x} 7f3a{i:04x} 00000000 deadbeef 0badf00d\n", 0x3000 + 16 * i)).collect();
        assert!(squeeze(&format!("{pad}\n{dump}"), true).contains("same pattern"));
        // a block of lines per case whose numbers wander is a table too
        let cases: String = (0..6)
            .map(|i| format!("case {i}\n  expected {}\n  got {}\n  diff {}\n", 200 + i * 7, 190 + i * 11, i * i))
            .collect();
        let text = format!("{pad}\n{cases}");
        assert_eq!(*squeeze(&text, true), text);
    }

    #[test]
    fn a_fold_says_repeats_only_of_the_same_lines() {
        let pad = "x".repeat(MIN_SAVING);
        let same = "movdqa xmm0, cs:xmmword_4340\nmovaps [rsp+var_98], xmm0\n".repeat(12);
        let s = squeeze(&format!("{pad}\n{same}"), true).into_owned();
        assert!(s.contains("block above repeats 11 more times"), "{s}");
        // blocks that differ in a counter are not the same block: the first and the last stay, and the
        // mark says only that they share a form
        let steps: String = (0..12)
            .map(|k| format!("movdqa xmm0, cs:xmmword_{}\nmovaps [rsp+var_{}], xmm0\n", 4340 + 10 * k, 200 - 10 * k))
            .collect();
        let s = squeeze(&format!("{pad}\n{steps}"), true).into_owned();
        assert!(!s.contains("repeats"), "{s}");
        assert!(s.contains("xmmword_4340") && s.contains("var_90") && s.contains("+10 more 2-line blocks"), "{s}");
    }

    #[test]
    fn without_a_way_back_only_exact_folds() {
        let pad = "x".repeat(MIN_SAVING);
        // lines that only share a form go as that form and a row of what differs per line, from which each
        // one comes back
        let log: String = (0..30)
            .map(|i| format!("[12:00:{i:02}] worker 3 handled request {i} in 40 ms, id 7f3a{i:04x}\n"))
            .collect();
        let s = squeeze(&format!("{pad}\n{log}"), false).into_owned();
        let head = "[curator] 30 lines of the form «[{}:{}:{}] worker {} handled request {} in {} ms, id {}»; each row below holds what fills its {}, in order:";
        let form = head.split('«').nth(1).unwrap().split('»').next().unwrap();
        let rows: Vec<&str> = s.lines().skip_while(|l| *l != head).skip(1).take(30).collect();
        let back: String = rows
            .iter()
            .map(|r| {
                let mut v = r.split(' ');
                let parts = form.split("{}").enumerate();
                let line: String = parts
                    .map(|(k, p)| if k == 0 { p.to_string() } else { format!("{}{p}", v.next().unwrap()) })
                    .collect();
                line + "\n"
            })
            .collect();
        assert_eq!(back, log, "{s}");
        // a table of measurements stays as it is: its numbers are what was asked for
        let sweep: String = (0..30).map(|i| format!("run {i}: loss {}\n", 1000 / (i + 3))).collect();
        let text = format!("{pad}\n{sweep}");
        assert_eq!(*squeeze(&text, false), text);
        // the same line over and over goes as its count, which brings each one back
        let s = squeeze(&format!("{pad}\n{}", "retrying connection to the server\n".repeat(40)), false).into_owned();
        assert!(s.contains("×39 more identical lines"), "{s}");
        // a long line stays whole
        let long = format!("{pad}\n{}", "ab".repeat(2000));
        assert_eq!(*squeeze(&long, false), long);
        // a long run of one form is clipped only with a way back, and the mark says what is missing only
        let rows: String = (0..400).map(|i| format!("row {i} value {}\n", i * 31 % 17)).collect();
        assert_eq!(*squeeze(&rows, false), rows);
        let s = squeeze(&rows, true).into_owned();
        assert!(s.contains("more lines of the same form)") && !s.contains("re-run"), "{s}");
    }

    #[test]
    fn redraws_and_binary_data() {
        // a progress bar drawn again in place keeps its last frame
        let bar: String = (0..=50).map(|i| format!("\r{:>3}% [{:<50}]", i * 2, "#".repeat(i))).collect();
        let text = format!("downloading\n{bar}\ndone\n");
        assert_eq!(squeeze(&text, false), format!("downloading\n100% [{}]\ndone\n", "#".repeat(50)));
        // a status block drawn again by moving the cursor up keeps its last frame too
        let frames: String =
            (0..20).map(|i| format!("\x1b[2A\x1b[2K\rlayer a: {}%\n\x1b[2K\rlayer b: {}%\n", i * 5, i * 3)).collect();
        let s = squeeze(&format!("pulling\n\n\n{frames}ok\n"), false).into_owned();
        assert_eq!(s, "pulling\nlayer a: 95%\nlayer b: 57%\nok\n");
        // a run of one character that is not text goes as its count, exactly
        let zeros = format!("header{}tail\n{}", "\0".repeat(300), "y\n".repeat(3));
        let s = squeeze(&zeros, false).into_owned();
        assert!(s.starts_with("header[curator](300×U+0000)tail"), "{s}");
        // with a way back, a stretch of mixed binary data goes as its length; its words of text stay
        let junk: String = (0..400).map(|i| if i % 3 == 0 { '\u{FFFD}' } else { 'a' }).collect();
        let s = squeeze(&format!("ELF{junk} libc.so.6 {junk}\n"), true).into_owned();
        assert!(s.contains("chars of binary data)") && s.contains("libc.so.6"), "{s}");
    }

    #[test]
    fn repeats_apart_point_to_their_first_copy() {
        let warn = "warning: the function `parse_config` is deprecated since 2.1; see the changelog for details\n  --> src/config/loader.ts:41:7\n   |\n41 |     const cfg = parse_config(raw, { strict: true, env: process.env.NODE_ENV });\n   |                 ^^^^^^^^^^^^\n";
        let text: String = (0..6).map(|i| format!("compiling module {i}\n{warn}")).collect();
        let s = squeeze(&text, false).into_owned();
        assert_eq!(s.matches("is deprecated").count(), 1, "{s}");
        assert_eq!(
            s.matches("again the 5 lines above from the first one that starts «warning: the function").count(),
            5,
            "{s}"
        );
        assert!(s.contains("compiling module 5"), "{s}");
        // a short line is cheaper than a pointer: it stays
        let short: String = (0..40).map(|i| format!("step {i}\nok\n")).collect();
        assert!(!squeeze(&short, false).contains("again"));
    }

    /// A name made of letters only, so lines that carry it differ in their form, not only in a number.
    fn letters(mut k: usize) -> String {
        let mut s = String::new();
        loop {
            s.push((b'a' + (k % 26) as u8) as char);
            k /= 26;
            if k == 0 {
                return s;
            }
        }
    }

    #[test]
    fn a_line_that_stands_out_is_never_folded_away() {
        // a test run: hundreds of `ok`, one `FAILED`, every name different
        let run: Vec<String> =
            (0..400).map(|k| format!("test parser::tests::case_{} ... ok", letters(k + 700))).collect();
        let failed = run[217].replace("... ok", "... FAILED");
        let text = run.join("\n").replacen(&run[217], &failed, 1);
        let s = squeeze(&text, true).into_owned();
        assert!(s.contains(&failed), "{s}");
        assert_eq!(s.matches("more lines of the same form").count(), 2, "{s}");
        // a log: one ERROR among INFO lines that all say something else
        let log: Vec<String> =
            (0..300).map(|k| format!("INFO handled request from {} in cache", letters(k + 900))).collect();
        let error = log[150].replace("INFO handled", "ERROR dropped");
        let text = log.join("\n").replacen(&log[150], &error, 1);
        let s = squeeze(&text, true).into_owned();
        assert!(s.contains(&error), "{s}");
        // a second kind of line, not news: past what the head holds, it folds with the rest
        let listing: Vec<String> = (0..600)
            .map(|k| format!("{} 1 owner staff {}", if k % 3 == 0 { "drwxr-xr-x" } else { "-rw-r--r--" }, letters(k)))
            .collect();
        let s = squeeze(&listing.join("\n"), true).into_owned();
        assert!(s.lines().count() <= 3 * SHAPE_HEAD + SHAPE_TAIL + 1, "{}", s.lines().count());
    }

    #[test]
    fn a_carriage_return_writes_over_as_a_terminal_does() {
        // a shorter frame leaves what it does not cover, as a terminal shows it: nothing is lost
        assert_eq!(clean("error: the disk is full\rretrying"), "retryinghe disk is full");
        // a frame that erases the line first keeps only itself
        assert_eq!(clean("error: the disk is full\r\x1b[Kretrying"), "retrying");
        // a line ending in a carriage return and a line feed is one line
        assert_eq!(clean("one\r\ntwo\r\n"), "one\ntwo\n");
    }

    #[test]
    fn a_sequence_cut_off_takes_nothing_past_its_line() {
        assert_eq!(clean("a\x1b[31\nkept line\n"), "a\nkept line\n");
        assert_eq!(clean("\x1b]0;title cut\nkept line\n"), "\nkept line\n");
        assert_eq!(clean("end\x1b"), "end");
    }

    #[test]
    fn a_huge_cursor_move_stops_at_a_screen_edge() {
        // a move past any screen neither panics nor fills memory
        let right = clean("a\x1b[18446744073709551615Cx");
        assert!(right.len() < 2000 && right.starts_with('a') && right.ends_with('x'));
        let down = clean("a\x1b[4000000B\x1b[4000000Bx");
        assert!(down.lines().count() < 2000 && down.ends_with('x'));
        let column = clean("\x1b[4000000Gx");
        assert!(column.len() < 2000 && column.ends_with('x'));
        // the usual moves still work
        assert_eq!(clean("ab\x1b[3Cc"), "ab   c");
        assert_eq!(clean("a\x1b[2Bb"), "a\n\n b");
    }
}
