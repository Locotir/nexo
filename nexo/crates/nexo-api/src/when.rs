//! `--when EXPR` of `nexo api watch`: a small condition on picked values.
//!
//! ```text
//! $.price < 100          $.price < prev        $.stock == "in stock"
//! $.price changed        $.price <= 50 and $.stock != 0 or $.sale == "yes"
//! ```
//!
//! `PATH OP VALUE` (OP is `<`, `<=`, `>`, `>=`, `==` or `!=`) or `PATH changed`; clauses are joined by `and` / `or`,
//! read left to right, no parentheses. VALUE is a number, a quoted string, or `prev`: the value of the same path at the
//! last poll (or in the state file). With no previous poll a clause with `prev` or `changed` is false, so a first run
//! never alerts. Text such as `$1,299.00` or `1299 €` is read as a number; a format that could be read two ways
//! (`1.299,00`, `1.299`) is refused with a message instead of guessed.

use crate::select::{self, Path, Picked};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl Op {
    fn word(self) -> &'static str {
        match self {
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Eq => "==",
            Op::Ne => "!=",
        }
    }

    fn holds(self, ord: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::*;
        match self {
            Op::Lt => ord == Less,
            Op::Le => ord != Greater,
            Op::Gt => ord == Greater,
            Op::Ge => ord != Less,
            Op::Eq => ord == Equal,
            Op::Ne => ord != Equal,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Rhs {
    Num(f64),
    Str(String),
    Prev,
}

#[derive(Debug, Clone)]
enum Clause {
    Cmp(Path, Op, Rhs),
    Changed(Path),
}

/// A parsed condition.
#[derive(Debug, Clone)]
pub struct When {
    pub text: String,
    first: Clause,
    rest: Vec<(bool, Clause)>,
}

/// A value that moved: the concrete path, the value now and at the last poll.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub path: String,
    pub now: Option<Value>,
    pub prev: Option<Value>,
}

// ---------------------------------------------------------------- numbers

/// What may stand before (`before`) or after a number: a currency sign, or up to three letters set off by a space.
fn money_edge(s: &str, before: bool) -> bool {
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    let set_off =
        letters == 0 || if before { s.ends_with(char::is_whitespace) } else { s.starts_with(char::is_whitespace) };
    letters <= 3 && set_off && s.chars().all(|c| c.is_whitespace() || c.is_ascii_alphabetic() || "$€£¥₹₽₩".contains(c))
}

fn groups_of_three(digits: &str) -> bool {
    let mut it = digits.split(',');
    let first = it.next().unwrap_or("");
    (1..=3).contains(&first.len()) && it.all(|g| g.len() == 3)
}

/// A number written for people: `1299`, `12.5`, `1,299.00`, `$1,299.00`, `1299 €`, `-3`. A text that could be read two
/// ways (`1.299,00`, `1.299`, `1,299`, `1.299.000`) is an error that says so.
pub fn parse_number(text: &str) -> Result<f64, String> {
    let t = text.trim();
    let is_num = |c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | ',');
    let start = t.find(is_num).ok_or_else(|| format!("{t:?} is not a number"))?;
    let end = t.rfind(|c: char| c.is_ascii_digit()).ok_or_else(|| format!("{t:?} is not a number"))? + 1;
    if end <= start || !money_edge(&t[..start], true) || !money_edge(&t[end..], false) {
        return Err(format!("{t:?} is not a number"));
    }
    let core = &t[start..end];
    let (neg, body) = match core.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, core.strip_prefix('+').unwrap_or(core)),
    };
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') {
        return Err(format!("{t:?} is not a number"));
    }
    let ambiguous = |why: &str| {
        format!("{t:?} is ambiguous ({why}); write it as plain digits with a dot for decimals, like 1299.00")
    };
    let dots = body.matches('.').count();
    let commas = body.matches(',').count();
    let plain = match (dots, commas) {
        (0, 0) => body.to_string(),
        (1, 0) => {
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            if frac.len() == 3 && int.len() <= 3 && int != "0" && !int.is_empty() {
                return Err(ambiguous("a dot may mark thousands or decimals"));
            }
            body.to_string()
        }
        (0, 1) => {
            let (int, frac) = body.split_once(',').unwrap_or((body, ""));
            if frac.len() == 3 {
                return Err(ambiguous("a comma may mark thousands or decimals"));
            }
            if int.is_empty() || frac.is_empty() {
                return Err(format!("{t:?} is not a number"));
            }
            format!("{int}.{frac}")
        }
        (0, _) => {
            if !groups_of_three(body) {
                return Err(format!("{t:?} is not a number"));
            }
            body.replace(',', "")
        }
        (1, _) => {
            let (head, frac) = body.rsplit_once('.').unwrap_or((body, ""));
            if body.rfind(',') > body.rfind('.') {
                return Err(ambiguous("the dot is before the comma, the European way"));
            }
            if frac.is_empty() || !groups_of_three(head) {
                return Err(format!("{t:?} is not a number"));
            }
            format!("{}.{frac}", head.replace(',', ""))
        }
        _ => return Err(ambiguous("dots may mark thousands")),
    };
    let n: f64 = plain.parse().map_err(|_| format!("{t:?} is not a number"))?;
    Ok(if neg { -n } else { n })
}

// ---------------------------------------------------------------- the parser

struct Scan<'a> {
    s: &'a str,
    at: usize,
}

impl<'a> Scan<'a> {
    fn skip(&mut self) {
        while self.s[self.at..].starts_with(char::is_whitespace) {
            self.at += self.s[self.at..].chars().next().map_or(1, char::len_utf8);
        }
    }

    fn done(&mut self) -> bool {
        self.skip();
        self.at >= self.s.len()
    }

    fn rest(&self) -> &'a str {
        &self.s[self.at..]
    }

    /// A path: up to whitespace or an operator character outside brackets and quotes.
    fn path(&mut self) -> Result<Path, String> {
        self.skip();
        let start = self.at;
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        for (i, c) in self.rest().char_indices() {
            match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    '\'' | '"' => quote = Some(c),
                    '[' => depth += 1,
                    ']' => depth = depth.saturating_sub(1),
                    c if depth == 0 && (c.is_whitespace() || "<>=!".contains(c)) => {
                        self.at = start + i;
                        return Path::parse(&self.s[start..self.at]);
                    }
                    _ => {}
                },
            }
        }
        self.at = self.s.len();
        Path::parse(&self.s[start..])
    }

    fn word(&mut self) -> &'a str {
        self.skip();
        let r = self.rest();
        let end = r.find(char::is_whitespace).unwrap_or(r.len());
        self.at += end;
        &r[..end]
    }

    fn peek_word(&mut self) -> &'a str {
        let save = self.at;
        let w = self.word();
        self.at = save;
        w
    }

    fn op(&mut self) -> Result<Op, String> {
        self.skip();
        let r = self.rest();
        for (w, op) in [("<=", Op::Le), (">=", Op::Ge), ("==", Op::Eq), ("!=", Op::Ne), ("<", Op::Lt), (">", Op::Gt)] {
            if r.starts_with(w) {
                self.at += w.len();
                return Ok(op);
            }
        }
        let shown: String = r.chars().take(12).collect();
        Err(format!("expected one of < <= > >= == != or `changed` at {shown:?}"))
    }

    /// A value: a quoted string, `prev`, or the words up to the next `and` / `or` read as a number.
    fn rhs(&mut self) -> Result<Rhs, String> {
        self.skip();
        let r = self.rest();
        if let Some(q) = r.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let body = &r[1..];
            let end = body.find(q).ok_or_else(|| "a quoted string is not closed".to_string())?;
            self.at += end + 2;
            return Ok(Rhs::Str(body[..end].to_string()));
        }
        let mut words: Vec<&str> = Vec::new();
        loop {
            let w = self.peek_word();
            if w.is_empty() || (!words.is_empty() && matches!(w.to_ascii_lowercase().as_str(), "and" | "or")) {
                break;
            }
            words.push(self.word());
        }
        if words.is_empty() {
            return Err("a value is missing: a number, a quoted string or prev".to_string());
        }
        if words.len() == 1 && words[0].eq_ignore_ascii_case("prev") {
            return Ok(Rhs::Prev);
        }
        parse_number(&words.join(" "))
            .map(Rhs::Num)
            .map_err(|e| format!("{e}; a text value goes in quotes, like \"in stock\""))
    }

    fn clause(&mut self) -> Result<Clause, String> {
        let path = self.path()?;
        if self.peek_word().eq_ignore_ascii_case("changed") {
            self.word();
            return Ok(Clause::Changed(path));
        }
        let op = self.op()?;
        Ok(Clause::Cmp(path, op, self.rhs()?))
    }
}

impl When {
    /// Reads an expression; the error says what is wrong and where.
    pub fn parse(text: &str) -> Result<When, String> {
        let mut sc = Scan { s: text, at: 0 };
        if sc.done() {
            return Err("--when is empty".to_string());
        }
        let first = sc.clause().map_err(|e| format!("--when {text:?}: {e}"))?;
        let mut rest = Vec::new();
        while !sc.done() {
            let w = sc.word().to_ascii_lowercase();
            let and = match w.as_str() {
                "and" => true,
                "or" => false,
                other => return Err(format!("--when {text:?}: expected `and` or `or`, found {other:?}")),
            };
            rest.push((and, sc.clause().map_err(|e| format!("--when {text:?}: {e}"))?));
        }
        Ok(When { text: text.trim().to_string(), first, rest })
    }

    fn clauses(&self) -> impl Iterator<Item = &Clause> {
        std::iter::once(&self.first).chain(self.rest.iter().map(|(_, c)| c))
    }

    /// The paths the condition reads, each once.
    pub fn paths(&self) -> Vec<Path> {
        let mut out: Vec<Path> = Vec::new();
        for c in self.clauses() {
            let p = match c {
                Clause::Cmp(p, _, _) | Clause::Changed(p) => p,
            };
            if !out.iter().any(|x| x.text == p.text) {
                out.push(p.clone());
            }
        }
        out
    }

    /// Whether it holds for `doc`, given what was picked at the last poll (`None` on the first). The notes say why a
    /// value could not be compared (a text that is not a number); such a clause is false.
    pub fn eval(&self, doc: &Value, prev: Option<&Picked>) -> (bool, Vec<String>) {
        let mut notes = Vec::new();
        let mut acc = self.first.holds(doc, prev, &mut notes);
        for (and, c) in &self.rest {
            let v = c.holds(doc, prev, &mut notes);
            acc = if *and { acc && v } else { acc || v };
        }
        notes.dedup();
        (acc, notes)
    }

    /// The values the condition reads that differ from the last poll, or all of them when none was kept.
    pub fn moved(&self, now: &Picked, prev: Option<&Picked>) -> Vec<Moved> {
        let paths = self.paths();
        let mut out = Vec::new();
        for (path, v) in now {
            if !paths.iter().any(|p| p.matches(path)) {
                continue;
            }
            let was = prev.and_then(|p| p.iter().find(|(x, _)| x == path)).and_then(|(_, x)| x.clone());
            out.push(Moved { path: path.clone(), now: v.clone(), prev: was });
        }
        out
    }
}

fn num_of(v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| "a number out of range".to_string()),
        Value::String(s) => parse_number(s),
        other => Err(format!("{} is not a number", select::show(&Some(other.clone())))),
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn compare(path: &str, lhs: &Value, op: Op, rhs: &Rhs, prev: Option<&Value>, notes: &mut Vec<String>) -> bool {
    let mut say = |e: String| notes.push(format!("{path}: {e}"));
    match rhs {
        Rhs::Num(n) => match num_of(lhs) {
            Ok(x) => x.partial_cmp(n).is_some_and(|o| op.holds(o)),
            Err(e) => {
                say(e);
                false
            }
        },
        Rhs::Str(s) => op.holds(text_of(lhs).as_str().cmp(s.as_str())),
        Rhs::Prev => {
            let Some(p) = prev else { return false };
            match (num_of(lhs), num_of(p)) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b).is_some_and(|o| op.holds(o)),
                _ => match (lhs, p) {
                    (Value::String(a), Value::String(b)) => op.holds(a.cmp(b)),
                    _ if matches!(op, Op::Eq | Op::Ne) => (lhs == p) == (op == Op::Eq),
                    _ => {
                        say(format!("cannot order {} against {}", select::show(&Some(lhs.clone())), op.word()));
                        false
                    }
                },
            }
        }
    }
}

impl Clause {
    fn holds(&self, doc: &Value, prev: Option<&Picked>, notes: &mut Vec<String>) -> bool {
        match self {
            Clause::Changed(p) => {
                let Some(prev) = prev else { return false };
                let now = select::pick(doc, std::slice::from_ref(p));
                let before: Picked = prev.iter().filter(|(x, _)| p.matches(x) || *x == p.text).cloned().collect();
                !select::changes(&before, &now).is_empty()
            }
            Clause::Cmp(p, op, rhs) => {
                let now = select::pick(doc, std::slice::from_ref(p));
                now.iter().any(|(path, v)| {
                    let Some(v) = v else { return false };
                    let was = prev.and_then(|pv| pv.iter().find(|(x, _)| x == path)).and_then(|(_, x)| x.as_ref());
                    compare(path, v, *op, rhs, was, notes)
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn n(s: &str) -> f64 {
        parse_number(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn numbers_are_read_the_way_people_write_them() {
        assert_eq!(n("1299"), 1299.0);
        assert_eq!(n("1,299.00"), 1299.0);
        assert_eq!(n("$1,299.00"), 1299.0);
        assert_eq!(n("1299 €"), 1299.0);
        assert_eq!(n("12.5"), 12.5);
        assert_eq!(n("-3.25"), -3.25);
        assert_eq!(n("€ 12,50"), 12.5);
        assert_eq!(n("1,299,000"), 1299000.0);
        assert_eq!(n("0.125"), 0.125);
        assert_eq!(n("49.99 USD"), 49.99);
    }

    #[test]
    fn an_ambiguous_or_broken_number_is_refused_with_a_message() {
        let e = parse_number("1.299,00").unwrap_err();
        assert!(e.contains("ambiguous") && e.contains("plain digits"), "{e}");
        for bad in ["1.299", "1,299", "1.299.000", "abc", "", "12abc", "1,29,9.5", "version 2.0 beta", "--1"] {
            assert!(parse_number(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn the_expression_has_comparisons_changed_and_or() {
        let w = When::parse("$.price <= 1,299.00 and $.stock != 0 or $.sale == \"yes\"").unwrap();
        let texts: Vec<String> = w.paths().iter().map(|p| p.text.clone()).collect();
        assert_eq!(texts, ["$.price", "$.stock", "$.sale"]);
        assert!(When::parse("$.price<prev").is_ok());
        assert!(When::parse("$.price changed").is_ok());
        assert!(When::parse("$.price $.x").is_err());
        assert!(When::parse("$.price <").is_err());
        assert!(When::parse("$.price < 1.299,00").unwrap_err().contains("ambiguous"));
        assert!(When::parse("$.price < 5 xor $.a == 1").is_err());
        assert!(When::parse("").is_err());
        assert!(When::parse("price < 5").is_err());
    }

    #[test]
    fn a_number_value_may_hold_a_currency_word() {
        let w = When::parse("$.price < 1299 € and $.a > 1").unwrap();
        assert!(w.eval(&json!({"price": 1000, "a": 2}), None).0);
        assert!(!w.eval(&json!({"price": 1400, "a": 2}), None).0);
    }

    #[test]
    fn prev_is_the_value_at_the_last_poll() {
        let w = When::parse("$.price < prev").unwrap();
        let paths = vec![Path::parse("$.price").unwrap()];
        let before = select::pick(&json!({"price": 99.0}), &paths);
        assert!(w.eval(&json!({"price": 89.0}), Some(&before)).0);
        assert!(!w.eval(&json!({"price": 99.0}), Some(&before)).0);
        assert!(!w.eval(&json!({"price": 120.0}), Some(&before)).0);
        // the first run has no previous value, so it never alerts
        assert!(!w.eval(&json!({"price": 1.0}), None).0);
        // prices that come as text
        let t = select::pick(&json!({"price": "$1,299.00"}), &paths);
        assert!(w.eval(&json!({"price": "$1,199.00"}), Some(&t)).0);
    }

    #[test]
    fn changed_and_the_joins_go_left_to_right() {
        let paths = vec![Path::parse("$.a").unwrap(), Path::parse("$.b").unwrap()];
        let before = select::pick(&json!({"a": 1, "b": 1}), &paths);
        let c = When::parse("$.a changed").unwrap();
        assert!(c.eval(&json!({"a": 2, "b": 1}), Some(&before)).0);
        assert!(!c.eval(&json!({"a": 1, "b": 2}), Some(&before)).0);
        assert!(!c.eval(&json!({"a": 2}), None).0);
        // (false and true) or true, read left to right
        let j = When::parse("$.a == 9 and $.b == 1 or $.b == 1").unwrap();
        assert!(j.eval(&json!({"a": 1, "b": 1}), None).0);
        // false or (true and false)
        let k = When::parse("$.a == 9 or $.b == 1 and $.a == 9").unwrap();
        assert!(!k.eval(&json!({"a": 1, "b": 1}), None).0);
    }

    #[test]
    fn text_values_compare_as_text_and_a_bad_number_is_a_note_not_a_crash() {
        let w = When::parse("$.s == \"in stock\"").unwrap();
        assert!(w.eval(&json!({"s": "in stock"}), None).0);
        assert!(!w.eval(&json!({"s": "sold out"}), None).0);
        let p = When::parse("$.price < 100").unwrap();
        let (hit, notes) = p.eval(&json!({"price": "1.299,00"}), None);
        assert!(!hit && notes.len() == 1 && notes[0].contains("ambiguous"), "{notes:?}");
        assert!(!p.eval(&json!({}), None).0);
    }

    #[test]
    fn a_wildcard_clause_holds_when_any_match_does() {
        let w = When::parse("$.items[*].price < 5").unwrap();
        assert!(w.eval(&json!({"items": [{"price": 9}, {"price": 4}]}), None).0);
        assert!(!w.eval(&json!({"items": [{"price": 9}]}), None).0);
    }

    #[test]
    fn moved_lists_the_values_the_condition_reads() {
        let w = When::parse("$.price < prev").unwrap();
        let paths = vec![Path::parse("$.price").unwrap(), Path::parse("$.name").unwrap()];
        let before = select::pick(&json!({"price": 99, "name": "x"}), &paths);
        let now = select::pick(&json!({"price": 89, "name": "y"}), &paths);
        let m = w.moved(&now, Some(&before));
        assert_eq!(m, vec![Moved { path: "$.price".into(), now: Some(json!(89)), prev: Some(json!(99)) }]);
    }
}
