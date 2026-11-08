//! The filter language: a small query evaluated over the documents.
//!
//! ```text
//! filter := clause ("OR" clause)*
//! clause := cond ("AND" cond)*
//! cond   := field op value
//! field  := owner | title | body | tag | size
//! op     := = | != | ~ | < | >
//! value  := a whole number, or "text" in double quotes
//! ```
//!
//! `AND` binds tighter than `OR`. Text has no escapes: the next double quote ends it. `~` means
//! "contains", ignoring case; `<` and `>` compare numbers only.

use crate::error::{Result, VaultError};
use crate::store::{DocId, Document, Store};
use crate::users::UserId;

/// The longest search term accepted.
pub const MAX_TERM: usize = 64;

/// What a condition looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Owner,
    Title,
    Body,
    Tag,
    Size,
}

/// How a condition compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Contains,
    Lt,
    Gt,
}

/// The right-hand side of a condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Num(i64),
    Text(String),
}

/// One `field op value` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cond {
    pub field: Field,
    pub op: CmpOp,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
    Number(i64),
    Op(CmpOp),
}

fn tokenize(src: &str) -> Result<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut text = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(ch) => text.push(ch),
                    None => return Err(VaultError::Malformed("unterminated text in the filter".into())),
                }
            }
            out.push(Token::Text(text));
        } else if c.is_ascii_digit() || c == '-' {
            let mut digits = String::new();
            digits.push(c);
            chars.next();
            while let Some(&d) = chars.peek() {
                if !d.is_ascii_digit() {
                    break;
                }
                digits.push(d);
                chars.next();
            }
            let n =
                digits.parse().map_err(|_| VaultError::Malformed(format!("bad number {digits:?} in the filter")))?;
            out.push(Token::Number(n));
        } else if c.is_ascii_alphabetic() {
            let mut word = String::new();
            while let Some(&w) = chars.peek() {
                if !w.is_ascii_alphanumeric() {
                    break;
                }
                word.push(w);
                chars.next();
            }
            out.push(Token::Word(word));
        } else {
            chars.next();
            let op = match c {
                '=' => CmpOp::Eq,
                '~' => CmpOp::Contains,
                '<' => CmpOp::Lt,
                '>' => CmpOp::Gt,
                '!' if chars.next() == Some('=') => CmpOp::Ne,
                other => return Err(VaultError::Malformed(format!("unexpected {other:?} in the filter"))),
            };
            out.push(Token::Op(op));
        }
    }
    Ok(out)
}

impl Field {
    fn from_word(w: &str) -> Result<Field> {
        match w.to_ascii_lowercase().as_str() {
            "owner" => Ok(Field::Owner),
            "title" => Ok(Field::Title),
            "body" => Ok(Field::Body),
            "tag" => Ok(Field::Tag),
            "size" => Ok(Field::Size),
            _ => Err(VaultError::Malformed(format!("unknown field {w:?}"))),
        }
    }

    fn is_number(self) -> bool {
        matches!(self, Field::Owner | Field::Size)
    }
}

fn parse_cond(tokens: &[Token], at: &mut usize) -> Result<Cond> {
    let mut next = || {
        let t = tokens.get(*at).cloned().ok_or_else(|| VaultError::Malformed("the filter ends too early".into()));
        *at += 1;
        t
    };
    let field = match next()? {
        Token::Word(w) => Field::from_word(&w)?,
        other => return Err(VaultError::Malformed(format!("expected a field, found {other:?}"))),
    };
    let op = match next()? {
        Token::Op(op) => op,
        other => return Err(VaultError::Malformed(format!("expected an operator, found {other:?}"))),
    };
    let value = match next()? {
        Token::Number(n) => Value::Num(n),
        Token::Text(s) => Value::Text(s),
        other => return Err(VaultError::Malformed(format!("expected a value, found {other:?}"))),
    };
    let numeric = matches!(value, Value::Num(_));
    if numeric != field.is_number() {
        return Err(VaultError::Invalid(format!("{field:?} does not compare with {value:?}")));
    }
    if matches!(op, CmpOp::Lt | CmpOp::Gt) && !numeric || op == CmpOp::Contains && numeric {
        return Err(VaultError::Invalid(format!("{op:?} does not fit {value:?}")));
    }
    Ok(Cond { field, op, value })
}

impl Cond {
    /// Whether the document satisfies this condition.
    pub fn matches(&self, doc: &Document) -> bool {
        match (&self.value, self.field) {
            (Value::Num(n), Field::Owner) => compare(i64::from(doc.owner), self.op, *n),
            (Value::Num(n), _) => compare(doc.size() as i64, self.op, *n),
            (Value::Text(t), Field::Title) => text_matches(&doc.title, self.op, t),
            (Value::Text(t), Field::Body) => text_matches(&doc.body, self.op, t),
            (Value::Text(t), _) => {
                let hit = doc
                    .tags
                    .iter()
                    .any(|tag| text_matches(tag, if self.op == CmpOp::Ne { CmpOp::Eq } else { self.op }, t));
                hit != (self.op == CmpOp::Ne)
            }
        }
    }
}

fn compare(have: i64, op: CmpOp, want: i64) -> bool {
    match op {
        CmpOp::Eq => have == want,
        CmpOp::Ne => have != want,
        CmpOp::Lt => have < want,
        CmpOp::Gt => have > want,
        CmpOp::Contains => false,
    }
}

fn text_matches(have: &str, op: CmpOp, want: &str) -> bool {
    let (have, want) = (have.to_lowercase(), want.to_lowercase());
    match op {
        CmpOp::Eq => have == want,
        CmpOp::Ne => have != want,
        CmpOp::Contains => have.contains(&want),
        CmpOp::Lt | CmpOp::Gt => false,
    }
}

/// A parsed filter: alternatives of conditions that must all hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    clauses: Vec<Vec<Cond>>,
}

impl Filter {
    /// Whether the document satisfies at least one clause.
    pub fn matches(&self, doc: &Document) -> bool {
        self.clauses.iter().any(|clause| clause.iter().all(|c| c.matches(doc)))
    }

    /// How many `OR`-separated clauses the filter has.
    pub fn clause_count(&self) -> usize {
        self.clauses.len()
    }
}

/// Parses a filter.
pub fn parse(src: &str) -> Result<Filter> {
    let tokens = tokenize(src)?;
    let mut clauses = vec![Vec::new()];
    let mut at = 0;
    loop {
        let cond = parse_cond(&tokens, &mut at)?;
        clauses.last_mut().expect("one clause at least").push(cond);
        match tokens.get(at) {
            None => break,
            Some(Token::Word(w)) if w.eq_ignore_ascii_case("and") => at += 1,
            Some(Token::Word(w)) if w.eq_ignore_ascii_case("or") => {
                at += 1;
                clauses.push(Vec::new());
            }
            Some(other) => return Err(VaultError::Malformed(format!("expected AND or OR, found {other:?}"))),
        }
    }
    Ok(Filter { clauses })
}

/// The filter that finds the documents of `owner` whose title contains `term`.
pub fn search_filter(owner: UserId, term: &str) -> Result<String> {
    let term = term.trim();
    if term.is_empty() {
        return Err(VaultError::Invalid("the search term is empty".into()));
    }
    if term.chars().count() > MAX_TERM {
        return Err(VaultError::Invalid(format!("the search term is longer than {MAX_TERM} characters")));
    }
    Ok(format!("owner = {owner} AND title ~ \"{term}\""))
}

/// The ids of the documents in `store` that satisfy `filter`, in id order.
pub fn run(store: &Store, filter: &str) -> Result<Vec<DocId>> {
    let filter = parse(filter)?;
    Ok(store.iter().filter(|d| filter.matches(d)).map(|d| d.id).collect())
}

/// The ids of the documents of `owner` whose title contains `term`.
pub fn search(store: &Store, owner: UserId, term: &str) -> Result<Vec<DocId>> {
    run(store, &search_filter(owner, term)?)
}
