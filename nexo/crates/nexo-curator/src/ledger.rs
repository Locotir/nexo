//! The ledger: lines of the messages that left the view whose names are most likely to be needed again.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::LazyLock;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::hindsight::{words, Interner, Set, Sym, Tracer, UKind};
use crate::json::est_str;
use crate::msg::truthy;
use crate::textops::{self, Re};

pub static ERROR_MARKS: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^(Traceback \(most recent call last\)|\w*Error: |error: |FAIL: |FAILED |E   )|\b[1-9]\d* failed\b|(?i:^\s*\[?exit code[ \t]*:?[ \t]*[1-9]\d*\b|^(?![ \t]*(?-i:[✓✔√]|ok\s|PASS\b))[^\n]*?\b(?:failed|finished|exited) with exit code [1-9]\d*\b)",
        textops::M,
    )
});
const LINE_CHARS: usize = 240;
pub const POOL: usize = 6000;
/// Lines the ledger takes from one message at most (0: as many as pay).
pub const PER_MESSAGE: usize = 0;
/// What a ledger line adds to its text, in accurate tokens: the `m00001 ` label and the line break (4.3) and up to
/// one more that `est_str` rounds off.
const LINE_OVERHEAD: i64 = 6;
/// Rows of one form (a table, a listing, a log) that one message holds: the ledger keeps the first `FORM_KEEP`
/// of a run of `FORM_RUN` or more and says how many follow. The rows are on disk or one command away; the
/// first few name the file's shape, the rest would only fill the anchor.
pub const FORM_KEEP: usize = 4;
pub const FORM_RUN: usize = 8;
/// Rows of a table of values (`value_row`) that one run keeps: each row holds numbers no word stands for, so the
/// first few do not name the rest.
pub const VALUE_KEEP: usize = 80;
/// The worth of a line the person or the agent wrote, against a line a tool returned: what they typed names
/// what they mean to use again (a result names everything the tool saw).
pub const SAID: f64 = 2.0;
/// The worth of a line of a file the agent read or wrote, against a line a tool said: the file is on disk, one
/// read away, and the pins hold what was written whole. The ledger needs its name and its shape, not its body.
pub const ON_DISK: f64 = 0.3;

// A line of raw bytes: a hexdump or a disassembly (an address with a digit in it, then byte pairs), or any
// line with four or more byte-sized hex tokens.
static HEX_LINE: LazyLock<Re> =
    LazyLock::new(|| Re::new(r"^(?:0x[0-9a-fA-F]+|(?=[0-9a-fA-F]*\d)[0-9a-fA-F]{3,})[:>]?\s", 0));
static HEX_TOKEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<![\w.])[0-9a-fA-F]{2}(?![\w.])", 0));
// A line of disassembly without the bytes (objdump, gdb, radare), maybe behind a grep file name and line
// number: an address (`0x…` or three or more hex digits and a colon), maybe a `<symbol>`, maybe byte pairs,
// then a short lower-case mnemonic that either ends the line (`ret`, `leave`) or takes ...
static ASM_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*(?:[\w./\\-]+:)?(?:\d+[:-]\s*)?(?:0x[0-9a-fA-F]+|[0-9a-fA-F]{3,}:)\s*(?:<[^>]*>:?\s*)?(?:[0-9a-fA-F]{2}\s+)*[a-z][a-z0-9.]{0,7}(?:\s+|$)",
        0,
    )
});
// ... an operand that only machine code has: a register, an immediate, an address, a symbol, a memory operand
static ASM_OPERAND: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"%[a-z]|\$0x|\b0x[0-9a-fA-F]+|<[\w@.+-]+>|\[[^\]]+\]|\b(?:r\d{1,2}[dwb]?|[re]?[abcd]x|[re]?[sb]p|[re]?[sd]i|[xw]\d{1,2}|sp|lr|pc)\b",
        0,
    )
});
static ALPHA: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^\W\d_]{3,}", 0));
// A row of a hexdump in words (`xxd -g 4`, `od -x`, gdb's `x/8xw`): three or more groups of exactly eight hex digits.
static HEX_WORD: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<![\w.])[0-9a-fA-F]{8}(?![\w.])", 0));
// What a binary says about itself, none of which the agent types back: a section name on its own (`.eh_frame`,
// `.note.ABI-tag`), a disassembly label (`0000000000001000 <.init>:`), a call through the PLT, an `nm` row
// (`U __isoc99_sscanf@GLIBC_2.7`, `0000000000004010 B stdout@GLIBC_2.2.5`).
static BINARY_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*(?:\.(?:text|data|bss|rodata|rela?\.[\w.]+|eh_frame(?:_hdr)?|(?:pre)?init(?:_array)?|fini(?:_array)?|plt(?:\.got|\.sec)?|got(?:\.plt)?|dynsym|dynstr|dynamic|interp|comment|note\.[\w.-]+|shstrtab|strtab|symtab|gnu[\w.]*|tbss|tdata|debug_\w+|ctors|dtors|jcr|hash|data\.rel\.ro)|(?:0x)?[0-9a-fA-F]{4,}\s+<[^>]*>:?|.*<[\w.]+@plt>.*|(?:[0-9a-fA-F]{8,}\s+)?[A-Za-z-]\s+_*[A-Za-z][\w.$]*@[\w.]+|(?:[0-9a-fA-F]{8,}\s+)?[A-Za-z]\s+__[\w.$@]+)\s*$",
        0,
    )
});
// A number no one types back: an address or a pointer in decimal (fourteen digits or more). One with a zero in
// front is an address in hex padded to sixteen digits, as `nm` and `readelf` print them, next to a name.
static BIG_NUMBER: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?<![\w.])[1-9]\d{13,}(?![\w.])", 0));
// The line number a file viewer puts before each line (`cat -n`, `grep -n`, `nl`, a read tool's `  12│`,
// `12→` or `12 | `): the number is not part of the line, and a line read from a binary dump must be judged
// without it.
static VIEWER: LazyLock<Re> = LazyLock::new(|| Re::new(r"^\s*\d+(?:\t|→|│|:\s|\s\|\s)", 0));
// The mark a tool leaves where it cut its own output (`... [12395 characters truncated] ...`, `(40 lines omitted)`):
// it says nothing about the work, and the cut is not what the agent will look for again.
static CUT_MARK: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"(?i)^[\s\[(.…*-]*(?:\d[\d,]*\s+(?:more\s+)?(?:characters?|chars|bytes|lines|tokens)\s+)?(?:output\s+)?(?:truncated|omitted|elided|cut off)\b[^\n]{0,40}$|^[\s\[(.…*-]*(?:output\s+|lines?\s+)?[^\n]{0,24}\b\d[\d,]*\s+(?:more\s+)?(?:characters?|chars|bytes|lines|tokens)\s+(?:truncated|omitted|elided|cut off|skipped)\b[^\n]{0,24}$",
        0,
    )
});
// A row of `ls -l` for the directory itself or its parent.
static DOT_ROW: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[d-][rwxsStT-]{9}[+@.]?\s.*\s\.\.?$", 0));

// A value a table holds: a decimal, or an integer of four digits or more, as a whole token (`689.07`, `4373`).
static VALUE: LazyLock<Re> = LazyLock::new(|| Re::new(r"^[-+]?(?:\d+\.\d+|\d{4,})$", 0));

/// A row of a table of values: delimited by `|` or `,`, nothing in it but numbers and bars, and two or more
/// values (`| 4.62 | 157 | 689.07 |`). Its numbers are results (prices, totals, measurements) the agent may
/// be asked to reproduce; a row of small counters, years, line numbers or timestamps holds none.
pub fn value_row(ln: &str) -> bool {
    if !ln.contains(['|', ',']) {
        return false;
    }
    let mut values = 0;
    for t in ln.split(|c: char| c.is_whitespace() || c == '|' || c == ',').filter(|t| !t.is_empty()) {
        if VALUE.is_match(t) {
            values += 1;
        } else if !t.chars().all(|c| c.is_ascii_digit() || c == '-' || c == '.') {
            return false;
        }
    }
    values >= 2
}

/// A line of raw bytes: a hexdump, a disassembly (with or without the bytes), what a binary says about itself.
fn raw_bytes(ln: &str) -> bool {
    let bytes = HEX_TOKEN.find_all(ln).len();
    // the pattern is anchored with `^` and has no multiline flag: a search is a match at the start
    if bytes >= 4 || (bytes >= 2 && HEX_LINE.is_match(ln)) {
        return true;
    }
    // the mnemonic ends the line, or an operand follows it (`re.search(ln, pos)` after a space: `\b` agrees)
    if let Some((_, end)) = ASM_LINE.search(ln).and_then(|c| c[0]) {
        if end == ln.len() || ASM_OPERAND.is_match(&ln[end..]) {
            return true;
        }
    }
    HEX_WORD.find_all(ln).len() >= 3 || BINARY_LINE.is_match(ln)
}

/// A line the ledger should not carry: raw bytes (a hexdump, a disassembly, with or without the bytes) or data
/// with hardly a word in it (a table of numbers, a listing of one token), none of whose names the agent will
/// type back.
pub fn noise(ln: &str) -> bool {
    if raw_bytes(ln) || DOT_ROW.is_match(ln) || CUT_MARK.is_match(ln) {
        return true;
    }
    let tokens: Vec<&str> = textops::split_ws(ln).collect();
    // one short token mixing cases, digits or symbols (`HkD$`, `AkZ4`): a string pulled out of a binary
    if tokens.len() == 1 && textops::len(ln) <= 6 {
        let (up, low, other) = ln.chars().fold((0, 0, 0), |(u, l, o), c| {
            if c.is_ascii_uppercase() {
                (u + 1, l, o)
            } else if c.is_ascii_lowercase() {
                (u, l + 1, o)
            } else {
                (u, l, o + 1)
            }
        });
        let mixed = up > 0 && low > 0 && (other > 0 || !ln.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
        if mixed || (up > 0 && other > 0 && low == 0 && textops::len(ln) <= 5) {
            return true;
        }
    }
    let alpha = tokens.iter().filter(|t| ALPHA.is_match(t)).count();
    // a pointer or an address in decimal with a word or two around it (`strtol(0) -> 93824992252144`)
    if alpha <= 2 && BIG_NUMBER.is_match(ln) {
        return true;
    }
    // a row of numbers with a label or two is data (an assignment, `NAME = [...]`, is a fact: it stays); a row
    // of values (a table of prices, totals, measurements) is not: each value is a fact no word stands for
    if value_row(ln) {
        return false;
    }
    alpha == 0 || (tokens.len() >= 4 && alpha * 10 < tokens.len() * 3 && !ln.contains('='))
}

/// What is known about every distinctive name up to a message.
#[derive(Default)]
pub struct Names {
    /// blocks, typed, asked, results, first, last, crowd, errors
    pub stats: FxHashMap<Sym, [i64; 8]>,
    used: FxHashMap<Sym, i64>,
    units: usize,
    steps: usize,
}

impl Names {
    pub fn extend(&mut self, tr: &Tracer, upto: usize) {
        while self.units < tr.units.len() && tr.units[self.units].msg < upto {
            let i = self.units;
            let u = &tr.units[i];
            self.units += 1;
            if u.kind == UKind::Thinking {
                continue;
            }
            let err = u.kind == UKind::Result
                && (truthy(tr.block(i).get("is_error")) || ERROR_MARKS.is_match(&tr.unit_text(i)));
            let n = u.words.len() as i64;
            for &w in &u.words {
                let st = self.stats.entry(w).or_insert([0, 0, 0, 0, u.msg as i64, u.msg as i64, n, 0]);
                st[0] += 1;
                st[6] = st[6].min(n);
                st[1] += matches!(u.kind, UKind::Call | UKind::Text) as i64;
                st[2] += (u.kind == UKind::User) as i64;
                st[3] += (u.kind == UKind::Result) as i64;
                st[7] += err as i64;
                st[5] = u.msg as i64;
            }
        }
        while self.steps < tr.steps.len() && tr.steps[self.steps] < upto {
            if let Some(ws) = tr.used.get(&tr.steps[self.steps]) {
                for &w in ws {
                    *self.used.entry(w).or_insert(0) += 1;
                }
            }
            self.steps += 1;
        }
    }

    pub fn features(&self, w: Sym, t: usize, int: &Interner) -> [f64; 11] {
        let st = &self.stats[&w];
        let s = int.get(w);
        let l = |x: i64| (x as f64).ln_1p();
        [
            l(st[0]),
            l(st[1]),
            l(st[2]),
            l(st[3]),
            l(*self.used.get(&w).unwrap_or(&0)),
            l((t as i64 - st[5]).max(0)),
            l(st[5] - st[4]),
            if s.contains('/') { 1.0 } else { 0.0 },
            if textops::first(s).is_some_and(textops::is_digit) { 1.0 } else { 0.0 },
            (textops::len(s) as f64).ln(),
            l(st[6]),
        ]
    }
}

pub fn prior(x: &[f64; 11]) -> f64 {
    let z = -2.5 + 0.8 * x[1] + 0.8 * x[2] + 0.5 * x[0] + 0.7 * x[4] - 0.3 * x[5];
    1.0 / (1.0 + (-z).exp())
}

/// The lines of a text that hold distinctive names, with those names. Lines of raw bytes or bare data
/// (`noise`) are left out, judged without the line number a file viewer put before them.
pub fn lines_of(text: &str, int: &mut Interner) -> Vec<(String, Set)> {
    lines_but(text, int, noise)
}

/// The lines of what the person or the agent wrote (its text, a note): only raw bytes they pasted are left out.
/// A line of theirs with few words and many numbers is a measurement or a result they stated (`retries 3 -> 41 ms,
/// 5 -> 97 ms`), the very thing they would otherwise run again to see; a big number in it is a value they read.
pub fn said_lines(text: &str, int: &mut Interner) -> Vec<(String, Set)> {
    lines_but(text, int, raw_bytes)
}

/// The text without the line number a file viewer may have put before the line, and whether the viewer's mark
/// (`│`, `→`) says for sure that it is one (`12: ` may as well be an address in a disassembly).
fn unnumbered(ln: &str) -> (&str, bool) {
    match VIEWER.search(ln).and_then(|c| c[0]) {
        Some((_, end)) => (&ln[end..], ln[..end].ends_with(['│', '→'])),
        None => (ln, false),
    }
}

fn lines_but(text: &str, int: &mut Interner, skip: fn(&str) -> bool) -> Vec<(String, Set)> {
    let mut out = Vec::new();
    for ln in text.split('\n') {
        let (bare, sure) = unnumbered(ln);
        // a line with no viewer's number is its own bare text: squashed and tested once
        let numbered = bare.len() != ln.len();
        let whole = textops::squash_ws(ln);
        let bare = if numbered { textops::squash_ws(bare) } else { whole.clone() };
        if bare.is_empty() || skip(&whole) || (numbered && skip(&bare)) {
            continue;
        }
        let mut ln = if sure { bare } else { whole };
        if ln.is_empty() {
            continue;
        }
        if textops::len(&ln) > LINE_CHARS {
            // rfind(" ", 0, LINE_CHARS + 1): a space starting before char LINE_CHARS
            let head = textops::head(&ln, LINE_CHARS + 1);
            let cut = head.rfind(' ').map(|b| head[..b].chars().count());
            let n = match cut {
                Some(c) if c > LINE_CHARS / 2 => c,
                _ => LINE_CHARS,
            };
            ln = format!("{}…", textops::head(&ln, n));
        }
        let ws = words(&ln, int);
        if !ws.is_empty() {
            out.push((ln, ws));
        }
    }
    out
}

struct Item(f64, usize, i64);

impl PartialEq for Item {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Item {
    // a max-heap of the smallest (-worth, index) first
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

/// Lazy greedy selection of lines by worth per token, a line's worth times its weight. ``pool``: (message,
/// line, names, weight). ``per_message`` > 0: at most that many lines of one message.
pub fn select(
    pool: &[(usize, String, Set, f64)],
    p: &FxHashMap<Sym, f64>,
    covered: &FxHashSet<Sym>,
    budget: i64,
    least: f64,
    per_message: usize,
) -> Vec<(usize, String)> {
    let mut covered = covered.clone();
    let worth = |ws: &Set, covered: &FxHashSet<Sym>| -> f64 {
        ws.iter().filter(|w| !covered.contains(w)).map(|w| p.get(w).copied().unwrap_or(0.0)).sum()
    };
    let mut heap = BinaryHeap::new();
    for (k, (_, ln, ws, weight)) in pool.iter().enumerate() {
        let v = worth(ws, &covered);
        if v > 0.0 {
            let c = est_str(ln) + LINE_OVERHEAD;
            heap.push(Item(-v * weight / c as f64, k, c));
        }
    }
    let mut taken: Vec<usize> = Vec::new();
    let mut count: FxHashMap<usize, usize> = FxHashMap::default();
    let mut used = 0;
    while let Some(Item(_, k, c)) = heap.pop() {
        let (mi, _, ws, weight) = &pool[k];
        let d = worth(ws, &covered) * weight / c as f64;
        if let Some(top) = heap.peek() {
            if d < -top.0 - 1e-12 {
                if d > 0.0 {
                    heap.push(Item(-d, k, c));
                }
                continue;
            }
        }
        if d <= 0.0 || d < least {
            break;
        }
        if used + c > budget || (per_message > 0 && count.get(mi).copied().unwrap_or(0) >= per_message) {
            continue;
        }
        taken.push(k);
        used += c;
        *count.entry(*mi).or_insert(0) += 1;
        covered.extend(ws.iter().copied());
    }
    taken.sort_unstable();
    taken.into_iter().map(|k| (pool[k].0, pool[k].1.clone())).collect()
}

/// The form of a line: the class of its first four tokens (number, path, word) and the separators of the
/// first one, so the rows of one table or listing share a form whatever their values.
pub fn form(ln: &str) -> String {
    let mut out = String::new();
    for (k, t) in textops::split_ws(ln).take(4).enumerate() {
        let t = t.trim_start_matches(['-', '+']);
        if k == 0 {
            let mut last = ' ';
            for c in t.chars().filter(|c| !c.is_alphanumeric()) {
                if c != last && !out.contains(c) && out.len() < 2 {
                    out.push(c);
                }
                last = c;
            }
            out.push(' ');
        }
        out.push(if t.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            '0'
        } else if t.contains('/') {
            'P'
        } else {
            'w'
        });
    }
    out
}

/// `lines` with every run of `FORM_RUN` or more consecutive lines of one form cut to its first `FORM_KEEP`, the
/// last of them saying how many more there were. Plain words (no mark in the first token, no number, no path)
/// are prose, not rows: they never fold.
pub fn fold_forms(lines: Vec<(String, Set)>) -> Vec<(String, Set)> {
    let forms: Vec<String> = lines.iter().map(|(ln, _)| form(ln)).collect();
    let prose = |f: &str| f.strip_prefix(' ').is_some_and(|r| r.chars().all(|c| c == 'w'));
    let mut out = Vec::with_capacity(lines.len());
    let mut a = 0;
    while a < lines.len() {
        let mut b = a + 1;
        while b < lines.len() && forms[b] == forms[a] {
            b += 1;
        }
        let values = lines[a..b].iter().all(|(ln, _)| value_row(ln));
        if b - a < FORM_RUN || prose(&forms[a]) || (values && b - a <= VALUE_KEEP) {
            out.extend_from_slice(&lines[a..b]);
        } else if values {
            out.extend_from_slice(&lines[a..a + VALUE_KEEP - 1]);
            let (ln, ws) = &lines[a + VALUE_KEEP - 1];
            out.push((format!("{ln} … (+{} more rows of the same form)", b - a - VALUE_KEEP), ws.clone()));
        } else {
            out.extend_from_slice(&lines[a..a + FORM_KEEP - 1]);
            let (ln, ws) = &lines[a + FORM_KEEP - 1];
            out.push((format!("{ln} … (+{} more rows of the same form)", b - a - FORM_KEEP), ws.clone()));
        }
        a = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_of_one_form_fold_to_their_first_four() {
        let mut int = Interner::default();
        let table: String = (0..20).map(|i| format!("item_{i}|slot{i}|{i}|ATK+{i};HP+{}\n", i * 3)).collect();
        let text = format!("data/items.dat holds the items:\n{table}parse_items in tools/parse.rs reads them\n");
        let folded = fold_forms(lines_of(&text, &mut int));
        let lines: Vec<&str> = folded.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(lines.len(), 6, "{lines:?}");
        assert_eq!(lines[0], "data/items.dat holds the items:");
        assert!(lines[4].ends_with("… (+16 more rows of the same form)"), "{}", lines[4]);
        assert_eq!(lines[5], "parse_items in tools/parse.rs reads them");
        // a short table stays whole
        let short: String = (0..7).map(|i| format!("item_{i}|slot|{i}|ATK+1\n")).collect();
        assert_eq!(fold_forms(lines_of(&short, &mut int)).len(), 7);
    }

    #[test]
    fn prose_lines_are_not_folded_as_a_table() {
        let mut int = Interner::default();
        let prose: String = (0..12)
            .map(|i| {
                format!(
                    "the {} module keeps order_book_{i} apart from the Billing rest\n",
                    ["cache", "queue", "store"][i % 3]
                )
            })
            .collect();
        let folded = fold_forms(lines_of(&prose, &mut int));
        assert_eq!(folded.len(), 12, "{folded:?}");
        assert!(folded.iter().all(|(l, _)| !l.contains("more rows")), "{folded:?}");
        // a table split by prose folds run by run: two runs of 6 stay whole, a run of 10 folds on its own
        let rows =
            |a: usize, b: usize| -> String { (a..b).map(|i| format!("item_{i}|slot{i}|{i}|ATK+{i}\n")).collect() };
        let text = format!(
            "{}the cache module keeps order_book apart\n{}the queue module keeps order_queue apart\n{}",
            rows(0, 6),
            rows(6, 12),
            rows(12, 22)
        );
        let folded = fold_forms(lines_of(&text, &mut int));
        let lines: Vec<&str> = folded.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(lines.len(), 6 + 1 + 6 + 1 + 4, "{lines:?}");
        assert!(lines[17].ends_with("… (+6 more rows of the same form)"), "{lines:?}");
    }

    #[test]
    fn a_form_is_blind_to_values() {
        assert_eq!(form("mormi_blade|head|16|GLD+13"), form("quingal_mail|feet|25|ATK+6;HP+35"));
        assert_eq!(
            form("-rw-r--r-- 1 root root 8490 Oct 29 18:35 enemies.dat"),
            form("-rwxr-xr-x 1 root root 18648 Oct 29 18:35 realm")
        );
        assert_ne!(form("cd /tmp/x && cat data/items.dat"), form("mormi_blade|head|16|GLD+13"));
    }
}

#[cfg(test)]
mod noise_tests {
    use super::noise;

    #[test]
    fn strings_pulled_out_of_a_binary_are_noise_and_names_are_not() {
        for s in ["HkD$", "AkZ4", "IcR0", "PTE1", "fXb9", ";nameH"] {
            assert!(noise(s), "{s}");
        }
        for s in ["GLIBC_2.7", "__printf_chk", "Makefile", "README", "main.rs", "Cargo", "nexo", "hero.sav"] {
            assert!(!noise(s), "{s}");
        }
    }

    #[test]
    fn prose_in_any_script_is_words_not_data() {
        for s in [
            "Функция parse_config падает при пустом вводе",
            "Η συνάρτηση parse_config αποτυγχάνει με κενή είσοδο",
            "parse_config 함수가 빈 입력에서 실패합니다 지금",
            "空の入力で parse_config が失敗します",
        ] {
            assert!(!noise(s), "{s}");
        }
    }

    #[test]
    fn what_a_binary_says_about_itself_is_noise() {
        for s in [
            ".eh_frame",
            ".note.ABI-tag",
            ".init_array",
            "0000000000001000 <.init>:",
            "0000000000001030 <__cxa_finalize@plt>:",
            "    1034:\tjmp    *0x2fc6(%rip)        # 4000 <__cxa_finalize@plt>",
            "U __isoc99_sscanf@GLIBC_2.7",
            "                 U __stack_chk_fail@GLIBC_2.4",
            "0000000000004010 B stdout@GLIBC_2.2.5",
            "w __gmon_start__",
            "3010 65616c6d 3a206361 6e6e6f74 206f7065 ealm: cannot ope",
            "0x555555558040 <buf>:\t0x6c616572 0x6163203a 0x6f6e6e61 0x706f2074",
            "strtol(0) -> 93824992252144",
            "$1 = 140737488346688",
            "drwxr-xr-x 2 user user 4096 Jan  1 12:00 .",
            "drwxr-xr-x 9 user user 4096 Jan  1 12:00 ..",
        ] {
            assert!(noise(s), "{s}");
        }
        for s in [
            ".gitignore",
            ".env",
            ".cargo",
            "U turn",
            "drwxr-xr-x 2 user user 4096 Jan  1 12:00 src",
            "the file has 12345678 lines",
            "serial 2024010112345678 is the build",
            "fn main() {",
            "Finding: the save file hero.sav is read by load_game() in save.rs",
        ] {
            assert!(!noise(s), "{s}");
        }
    }

    #[test]
    fn a_symbol_of_the_binary_is_a_name_whatever_digits_its_address_has() {
        // `nm` rows: an address of decimal digits only is still an address in hex, not a pointer
        for s in [
            "0000000000401136 T check_key",
            "0000000000404048 B attempts_left",
            "00000000004011d0 T _ZN5Vault7unlockEi",
            "     5: 0000000000404040     8 OBJECT  GLOBAL DEFAULT   25 secret_flag",
        ] {
            assert!(!noise(s), "{s}");
        }
        assert!(noise("$1 = 140737488346688"));
    }
}

#[cfg(test)]
mod form_tests {
    use super::form;

    #[test]
    fn a_signed_number_is_a_number() {
        assert_eq!(form("L -3 K 19 S 20 elemental"), form("L 16 K 200 S 4 humanoid"));
        assert_eq!(form("+12 rows"), form("7 rows"));
        assert_ne!(form("L -3 K 19"), form("L x K 19"));
    }
}

#[cfg(test)]
mod viewer_tests {
    use super::*;

    fn kept(text: &str, said: bool) -> Vec<String> {
        let mut int = Interner::default();
        let out = if said { said_lines(text, &mut int) } else { lines_of(text, &mut int) };
        out.into_iter().map(|(l, _)| l).collect()
    }

    #[test]
    fn a_dump_read_through_a_file_viewer_is_still_noise() {
        // the same disassembly and hexdump lines, as `cat -n`, `grep -n` and read tools number them
        for prefix in ["", "     8│ ", "8\t", "  12→", "431: ", "7 | "] {
            for ln in
                ["1000: endbr64", "    1a4e: 8b 45 fc   mov -0x4(%rbp),%eax", "00002000: 01000200 6c657665 6c3d0000"]
            {
                let line = format!("{prefix}{ln}");
                assert!(kept(&line, false).is_empty(), "{line:?} was kept");
            }
        }
        // the line number goes, the line stays
        assert_eq!(
            kept("    27│ - a command's output is cut to its first 15000 characters", false),
            vec!["- a command's output is cut to its first 15000 characters"]
        );
        assert_eq!(kept("  12→let total = sum_items(bag);", false), vec!["let total = sum_items(bag);"]);
        // a number that may be an address stays
        assert_eq!(kept("86: let e = item_effects(id);", false), vec!["86: let e = item_effects(id);"]);
        // a line that only begins with a number is not numbered
        assert_eq!(kept("181 data/enemies.dat", false), vec!["181 data/enemies.dat"]);
    }

    #[test]
    fn what_the_agent_stated_keeps_its_numbers() {
        // rows of measurements and a big value, which a tool's output would lose as bare data
        let said = "luck_bonus 5 -> 112, 6 -> 115, 7 -> 118, 8 -> 121\nseed 93824992252144 gives 31\n1a4e: 8b 45 fc 89 c7 e8 00";
        assert!(noise("luck_bonus 5 -> 112, 6 -> 115, 7 -> 118, 8 -> 121"));
        assert!(noise("seed 93824992252144 gives 31"));
        assert_eq!(
            kept(said, true),
            vec!["luck_bonus 5 -> 112, 6 -> 115, 7 -> 118, 8 -> 121", "seed 93824992252144 gives 31"]
        );
    }
}

#[cfg(test)]
mod cut_tests {
    use super::noise;

    #[test]
    fn where_a_tool_cut_its_output_is_noise_and_a_sentence_about_cutting_is_not() {
        for s in [
            "... [12395 characters truncated] ...",
            "[5580 characters truncated]",
            "(40 lines omitted)",
            "... 1,204 more lines truncated ...",
            "[output truncated]",
            "… 300 bytes elided …",
        ] {
            assert!(noise(s), "{s}");
        }
        for s in [
            "the proxy truncated the body of every POST to 4096 bytes, so upload_chunk must resend the rest",
            "read_to_end returned 4096 bytes and the response was truncated by the proxy at /v1/upload",
            "Truncate the log in rotate_logs after 300 lines",
        ] {
            assert!(!noise(s), "{s}");
        }
    }
}

#[cfg(test)]
mod value_tests {
    use super::*;

    #[test]
    fn a_row_of_values_is_a_fact_and_a_row_of_counters_is_not() {
        for s in ["| 4.62 | 157 | 689.07 |", "4.62, 157, 689.07", "| 0.65 | 17 | 11.05 |", "| 5.0 | 2730 | 11602.5 |"] {
            assert!(value_row(s), "{s}");
            assert!(!noise(s), "{s}");
        }
        for s in
            ["| 1 | 2 | 3 |", "| 12 | 7 | 300 |", "|---|---|---|", "2026-11-08 12:00:01", "1.2.3", "| bolt | 4.62 |"]
        {
            assert!(!value_row(s), "{s}");
        }
        assert!(noise("| 1 | 2 | 3 |"));
    }

    #[test]
    fn a_table_of_values_folds_late() {
        let mut int = Interner::default();
        let rows = |n: usize| -> String {
            (0..n).map(|i| format!("| {}.{:02} | {} | {}.50 |\n", 1 + i, i, 20 + i, 100 + i)).collect()
        };
        let mut kept = |n: usize| fold_forms(lines_of(&rows(n), &mut int));
        assert_eq!(kept(40).len(), 40);
        assert_eq!(kept(VALUE_KEEP).len(), VALUE_KEEP);
        let long = kept(VALUE_KEEP + 30);
        assert_eq!(long.len(), VALUE_KEEP);
        assert!(long[VALUE_KEEP - 1].0.ends_with("(+30 more rows of the same form)"), "{:?}", long[VALUE_KEEP - 1]);
    }
}
