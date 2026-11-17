//! GraphQL operations found in a request. One URL (`/graphql`) carries many different calls, so the endpoint list
//! alone says nothing: what tells them apart is the operation, its kind and its name. This reads them from a POST with
//! a JSON body (one operation, or a batch in an array) and from a GET with `query` or `extensions` parameters,
//! including persisted queries that send only a hash.
//!
//! A `query` string only counts when it reads like GraphQL (it starts with `{` or with `query`, `mutation`,
//! `subscription` or `fragment`), so a search box that posts `{"query": "shoes"}` is not taken for GraphQL.

use crate::exchange::Exchange;
use serde_json::{Map, Value};

/// How many characters of a persisted query's hash go in its name.
const HASH_CHARS: usize = 8;

/// One GraphQL operation of a request.
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    /// `query`, `mutation` or `subscription` (`query` for a persisted query that sends no text).
    pub kind: &'static str,
    /// The operation name, `anonymous`, or `persisted:` and the start of the hash.
    pub name: String,
    /// Whether the request carried a persisted-query hash (`extensions.persistedQuery`).
    pub persisted: bool,
    /// Its place in a batch (0 when the request holds one operation).
    pub index: usize,
    /// The variables it sent, if any.
    pub variables: Option<Value>,
}

/// The GraphQL operations a request holds; empty when it is not a GraphQL call.
pub fn ops(x: &Exchange) -> Vec<Op> {
    let mut out = Vec::new();
    if x.method == "POST" {
        let json = x.req_body.as_ref().and_then(|b| b.json.as_ref());
        match json {
            Some(Value::Array(a)) => {
                for (i, e) in a.iter().enumerate() {
                    if let Some(op) = op_of(e, i) {
                        out.push(op);
                    }
                }
            }
            Some(v @ Value::Object(_)) => {
                if let Some(op) = op_of(v, 0) {
                    out.push(op);
                }
            }
            _ => {}
        }
    } else if x.method == "GET" {
        let mut m = Map::new();
        for (k, v) in x.url.query_pairs() {
            match &*k {
                "query" | "operationName" => {
                    m.insert(k.to_string(), Value::String(v.to_string()));
                }
                "variables" | "extensions" => {
                    if let Ok(parsed) = serde_json::from_str::<Value>(&v) {
                        m.insert(k.to_string(), parsed);
                    }
                }
                _ => {}
            }
        }
        if let Some(op) = op_of(&Value::Object(m), 0) {
            out.push(op);
        }
    }
    out
}

/// The operation one JSON object stands for, if it is one.
fn op_of(e: &Value, index: usize) -> Option<Op> {
    let o = e.as_object()?;
    let text = o.get("query").and_then(Value::as_str).filter(|q| looks_like_graphql(q));
    let persisted = o.get("extensions").and_then(|x| x.get("persistedQuery")).filter(|p| p.is_object());
    if text.is_none() && persisted.is_none() {
        return None;
    }
    let (kind, parsed) = match text {
        Some(t) => parse(t),
        None => ("query", None),
    };
    let given = o.get("operationName").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let name = if let Some(g) = given {
        g.to_string()
    } else if let Some(p) = parsed {
        p
    } else if let Some(p) = persisted {
        let hash = p.get("sha256Hash").and_then(Value::as_str).unwrap_or("");
        let short: String = hash.chars().take(HASH_CHARS).collect();
        format!("persisted:{short}")
    } else {
        "anonymous".to_string()
    };
    let variables = o.get("variables").filter(|v| !v.is_null()).cloned();
    Some(Op { kind, name, persisted: persisted.is_some(), index, variables })
}

/// Whether a text starts like a GraphQL document.
fn looks_like_graphql(q: &str) -> bool {
    let mut it = Scanner::new(q);
    it.skip_blank();
    match it.peek() {
        Some('{') => true,
        Some(_) => matches!(it.word().as_str(), "query" | "mutation" | "subscription" | "fragment"),
        None => false,
    }
}

/// The kind of the first operation of a document and its name, `None` when it has none (`{ ... }`, `query { ... }`).
/// Fragments before it are skipped.
fn parse(q: &str) -> (&'static str, Option<String>) {
    let mut it = Scanner::new(q);
    loop {
        it.skip_blank();
        match it.peek() {
            None => return ("query", None),
            Some('{') => return ("query", None),
            Some(_) => {}
        }
        let kind = match it.word().as_str() {
            "query" => "query",
            "mutation" => "mutation",
            "subscription" => "subscription",
            "fragment" => {
                it.skip_braces();
                continue;
            }
            _ => return ("query", None),
        };
        it.skip_blank();
        let name = it.word();
        return (kind, if name.is_empty() { None } else { Some(name) });
    }
}

/// A cursor over GraphQL text.
struct Scanner<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Scanner<'a> {
    fn new(s: &'a str) -> Self {
        Scanner { chars: s.chars().peekable() }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    /// Skips white space, commas and `#` comments.
    fn skip_blank(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == ',' || c == '\u{feff}' {
                self.chars.next();
            } else if c == '#' {
                while let Some(d) = self.chars.next() {
                    if d == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Reads a name (letters, digits, `_`); empty when the next character is none of those.
    fn word(&mut self) -> String {
        let mut w = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                w.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        w
    }

    /// Skips up to and through the first balanced `{ ... }`.
    fn skip_braces(&mut self) {
        let mut depth = 0usize;
        let mut opened = false;
        while let Some(c) = self.chars.next() {
            if c == '{' {
                depth += 1;
                opened = true;
            } else if c == '}' {
                depth = depth.saturating_sub(1);
                if opened && depth == 0 {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::Body;
    use serde_json::json;

    fn exchange(method: &str, url: &str, body: Option<Value>) -> Exchange {
        Exchange {
            seq: 0,
            method: method.to_string(),
            url: url::Url::parse(url).expect("a url"),
            req_headers: Vec::new(),
            req_body: body.map(|json| Body { mime: "application/json".into(), json: Some(json), size: 0 }),
            status: 200,
            resp_headers: Vec::new(),
            resp_body: None,
            kind: "fetch".into(),
            sent: Vec::new(),
            got: Vec::new(),
        }
    }

    fn post(body: Value) -> Exchange {
        exchange("POST", "https://x.test/graphql", Some(body))
    }

    #[test]
    fn a_named_query_gives_its_name_and_variables() {
        let x = post(json!({"query": "query GetUser($id: ID!) { user(id: $id) { name } }", "variables": {"id": "7"}}));
        let o = ops(&x);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].kind, "query");
        assert_eq!(o[0].name, "GetUser");
        assert!(!o[0].persisted);
        assert_eq!(o[0].index, 0);
        assert_eq!(o[0].variables, Some(json!({"id": "7"})));
    }

    #[test]
    fn a_mutation_and_a_subscription_have_their_kind() {
        let m = ops(&post(json!({"query": "mutation  AddItem { add { id } }"})));
        assert_eq!((m[0].kind, m[0].name.as_str()), ("mutation", "AddItem"));
        let s = ops(&post(json!({"query": "subscription OnTick { tick }"})));
        assert_eq!((s[0].kind, s[0].name.as_str()), ("subscription", "OnTick"));
        assert_eq!(s[0].variables, None);
    }

    #[test]
    fn anonymous_operations_are_called_anonymous() {
        for q in ["{ me { id } }", "query { me { id } }", "  query ($a: Int) { me(a: $a) { id } }", "mutation { x }"] {
            let o = ops(&post(json!({ "query": q })));
            assert_eq!(o.len(), 1, "{q}");
            assert_eq!(o[0].name, "anonymous", "{q}");
        }
        assert_eq!(ops(&post(json!({"query": "mutation { x }"})))[0].kind, "mutation");
    }

    #[test]
    fn comments_and_fragments_before_the_operation_are_skipped() {
        let q = "# fetch the feed\n  # more\nfragment F on User { id }\nquery Feed { feed { ...F } }";
        let o = ops(&post(json!({ "query": q })));
        assert_eq!(o[0].name, "Feed");
        assert_eq!(o[0].kind, "query");
    }

    #[test]
    fn the_operation_name_field_wins_over_the_text() {
        let o = ops(&post(json!({"query": "query A { a } query B { b }", "operationName": "B"})));
        assert_eq!(o[0].name, "B");
        let empty = ops(&post(json!({"query": "query A { a }", "operationName": ""})));
        assert_eq!(empty[0].name, "A");
        let null = ops(&post(json!({"query": "query A { a }", "operationName": null})));
        assert_eq!(null[0].name, "A");
    }

    #[test]
    fn a_persisted_query_is_named_by_its_hash_when_it_has_no_name() {
        let hash = "abcdef0123456789abcdef0123456789";
        let x =
            post(json!({"extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}}, "variables": {"a": 1}}));
        let o = ops(&x);
        assert_eq!(o.len(), 1);
        assert!(o[0].persisted);
        assert_eq!(o[0].name, "persisted:abcdef01");
        assert_eq!(o[0].kind, "query");
        assert_eq!(o[0].variables, Some(json!({"a": 1})));
        let named = post(json!({"operationName": "Home", "extensions": {"persistedQuery": {"sha256Hash": hash}}}));
        let n = ops(&named);
        assert_eq!(n[0].name, "Home");
        assert!(n[0].persisted);
    }

    #[test]
    fn a_batch_gives_one_operation_per_entry_with_its_place() {
        let x = post(json!([
            {"query": "query A { a }"},
            {"note": "not an operation"},
            {"query": "mutation B { b }"},
            {"extensions": {"persistedQuery": {"sha256Hash": "1234567890abcdef"}}}
        ]));
        let o = ops(&x);
        let got: Vec<(usize, &str, &str)> = o.iter().map(|p| (p.index, p.kind, p.name.as_str())).collect();
        assert_eq!(got, [(0, "query", "A"), (2, "mutation", "B"), (3, "query", "persisted:12345678")]);
    }

    #[test]
    fn a_get_with_query_and_variables_parameters_is_read() {
        let mut u = url::Url::parse("https://x.test/graphql").expect("a url");
        u.query_pairs_mut()
            .append_pair("query", "query Search($q: String) { search(q: $q) { id } }")
            .append_pair("variables", "{\"q\":\"cats\"}")
            .append_pair("operationName", "Search");
        let o = ops(&exchange("GET", u.as_str(), None));
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].name, "Search");
        assert_eq!(o[0].variables, Some(json!({"q": "cats"})));
        assert!(!o[0].persisted);
    }

    #[test]
    fn a_get_with_a_persisted_extension_is_read() {
        let mut u = url::Url::parse("https://x.test/graphql").expect("a url");
        u.query_pairs_mut()
            .append_pair("extensions", "{\"persistedQuery\":{\"version\":1,\"sha256Hash\":\"deadbeefcafe0000\"}}")
            .append_pair("variables", "not json");
        let o = ops(&exchange("GET", u.as_str(), None));
        assert_eq!(o.len(), 1);
        assert!(o[0].persisted);
        assert_eq!(o[0].name, "persisted:deadbeef");
        assert_eq!(o[0].variables, None);
    }

    #[test]
    fn a_plain_json_post_is_not_graphql() {
        assert!(ops(&post(json!({"name": "x", "age": 3}))).is_empty());
        assert!(ops(&post(json!({"query": "shoes"}))).is_empty());
        assert!(ops(&post(json!({"query": 5}))).is_empty());
        assert!(ops(&post(json!({"query": "queryable things"}))).is_empty());
        assert!(ops(&post(json!([1, 2, 3]))).is_empty());
        assert!(ops(&post(json!("text"))).is_empty());
        assert!(ops(&exchange("POST", "https://x.test/api", None)).is_empty());
        assert!(ops(&exchange("GET", "https://x.test/search?query=shoes", None)).is_empty());
    }

    #[test]
    fn other_methods_are_not_read() {
        let mut x = post(json!({"query": "{ a }"}));
        x.method = "PUT".into();
        assert!(ops(&x).is_empty());
    }
}
