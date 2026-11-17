//! How a list endpoint pages: a cursor, a next URL, a page number, an offset or a `Link` header. It is worked out from
//! the names of the request parameters of the calls made to one endpoint and from the fields of its response shape.
//! Only names and JSON paths are kept, never a value, so a cursor (which is opaque) cannot leak.

use crate::exchange::Exchange;
use crate::graphql;
use crate::shape::{Fmt, Shape};
use serde_json::Value;

/// Names are compared lower case without `_` and `-`.
const CURSOR: &[&str] = &[
    "cursor",
    "after",
    "before",
    "pagetoken",
    "nextpagetoken",
    "continuation",
    "continuationtoken",
    "nexttoken",
    "starttoken",
];
const PAGE: &[&str] = &["page", "pagenumber", "pageno", "pg"];
const OFFSET: &[&str] = &["offset", "skip", "start", "from"];
const SIZE: &[&str] = &["limit", "size", "pagesize", "perpage", "count", "rows", "maxresults", "first", "take", "num"];
const NEXT: &[&str] =
    &["next", "nextcursor", "nextpagetoken", "nexttoken", "cursor", "endcursor", "continuation", "nextpage", "nexturl"];

/// How the list of one endpoint pages.
#[derive(Debug, Clone, PartialEq)]
pub struct Paging {
    /// `cursor`, `next url`, `page`, `offset` or `link header`.
    pub style: &'static str,
    /// The request parameter that moves to the next page: a query name (`after`), `variables.after` for a GraphQL
    /// variable or `body.cursor` for a field of a JSON body.
    pub param: Option<String>,
    /// The parameter that sets the page size.
    pub size: Option<String>,
    /// The JSON path in the response that holds what the next request needs (`$.pageInfo.endCursor`).
    pub next: Option<String>,
    /// The JSON path of the list in the response (`$.data`, or `$` for a top-level array).
    pub items: Option<String>,
}

impl Paging {
    /// One line for an agent: `cursor via ?after, next at $.pageInfo.endCursor, items at $.data`.
    pub fn describe(&self) -> String {
        let mut s =
            if self.style == "link header" { "link header (rel=next)".to_string() } else { self.style.to_string() };
        if let Some(p) = &self.param {
            s.push_str(" via ");
            s.push_str(&show_param(p));
        }
        if let Some(n) = &self.next {
            s.push_str(", next at ");
            s.push_str(n);
        }
        if let Some(i) = &self.items {
            s.push_str(", items at ");
            s.push_str(i);
        }
        if let Some(z) = &self.size {
            s.push_str(", size ");
            s.push_str(&show_param(z));
        }
        s
    }
}

/// A query name gets its `?`; a variable or body field is shown as it is.
fn show_param(p: &str) -> String {
    if p.contains('.') {
        p.to_string()
    } else {
        format!("?{p}")
    }
}

/// A request parameter seen over the calls.
struct Param {
    norm: String,
    name: String,
    values: Vec<String>,
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| *c != '_' && *c != '-').flat_map(char::to_lowercase).collect()
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn push(out: &mut Vec<Param>, name: String, key: &str, value: Option<String>) {
    let idx = match out.iter().position(|p| p.name == name) {
        Some(i) => i,
        None => {
            out.push(Param { norm: norm(key), name, values: Vec::new() });
            out.len() - 1
        }
    };
    if let Some(v) = value {
        out[idx].values.push(v);
    }
}

/// The query parameters, the GraphQL variables and the top-level fields of a JSON body of every call.
fn params_of(calls: &[&Exchange]) -> Vec<Param> {
    let mut out = Vec::new();
    for x in calls {
        for (k, v) in x.url.query_pairs() {
            push(&mut out, k.to_string(), &k, Some(v.to_string()));
        }
        let operations = graphql::ops(x);
        if operations.is_empty() {
            if let Some(Value::Object(m)) = x.req_body.as_ref().and_then(|b| b.json.as_ref()) {
                for (k, v) in m {
                    push(&mut out, format!("body.{k}"), k, scalar(v));
                }
            }
        }
        for op in operations {
            if let Some(Value::Object(m)) = &op.variables {
                for (k, v) in m {
                    push(&mut out, format!("variables.{k}"), k, scalar(v));
                }
            }
        }
    }
    out
}

fn find<'a>(params: &'a [Param], set: &[&str]) -> Option<&'a Param> {
    params.iter().find(|p| set.contains(&p.norm.as_str()))
}

fn find_page(params: &[Param]) -> Option<&Param> {
    params.iter().find(|p| {
        PAGE.contains(&p.norm.as_str())
            || (p.norm == "p"
                && !p.values.is_empty()
                && p.values.iter().all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())))
    })
}

fn link_has_next(calls: &[&Exchange]) -> bool {
    calls.iter().any(|x| {
        x.resp_header("link").map_or(false, |l| {
            let l = l.to_ascii_lowercase();
            l.contains("rel=\"next\"") || l.contains("rel=next")
        })
    })
}

/// How the calls to one endpoint page, given the shape of its responses; `None` when nothing says they do.
pub fn detect(calls: &[&Exchange], resp: &Shape) -> Option<Paging> {
    let params = params_of(calls);
    // (path, whether it holds a URL)
    let mut nexts: Vec<(String, bool)> = Vec::new();
    // (path, longest array, outside the items of another array)
    let mut lists: Vec<(String, usize, bool)> = Vec::new();
    if resp.arrays > 0 && resp.max_items > 0 {
        lists.push(("$".to_string(), resp.max_items, true));
    }
    resp.walk(&mut |path, key, shape| {
        if NEXT.contains(&norm(key).as_str()) {
            nexts.push((path.to_string(), shape.strs > 0 && shape.fmt == Fmt::Uri));
        }
        if shape.arrays > 0 && shape.max_items > 0 {
            lists.push((path.to_string(), shape.max_items, !path.contains("[]")));
        }
    });
    let mut items: Option<(&str, usize, bool)> = None;
    for (p, n, top) in &lists {
        let better = match items {
            None => true,
            Some((_, bn, btop)) => (*top, *n) > (btop, bn),
        };
        if better {
            items = Some((p.as_str(), *n, *top));
        }
    }
    let items = items.map(|(p, _, _)| p.to_string());
    let size = find(&params, SIZE).map(|p| p.name.clone());
    let next_any = nexts.first().map(|n| n.0.clone());
    let make = |style: &'static str, param: Option<String>, next: Option<String>| Paging {
        style,
        param,
        size: size.clone(),
        next,
        items: items.clone(),
    };
    if let Some(p) = find(&params, CURSOR) {
        return Some(make("cursor", Some(p.name.clone()), next_any));
    }
    if let Some(n) = nexts.iter().find(|n| n.1) {
        return Some(make("next url", None, Some(n.0.clone())));
    }
    if let Some(p) = find_page(&params) {
        return Some(make("page", Some(p.name.clone()), next_any));
    }
    if let Some(p) = find(&params, OFFSET) {
        return Some(make("offset", Some(p.name.clone()), next_any));
    }
    if link_has_next(calls) {
        return Some(make("link header", None, None));
    }
    if next_any.is_some() {
        return Some(make("cursor", None, next_any));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::{opaque, Body};
    use serde_json::json;

    fn call(url: &str) -> Exchange {
        Exchange {
            seq: 0,
            method: "GET".into(),
            url: url::Url::parse(url).expect("a url"),
            req_headers: Vec::new(),
            req_body: None,
            status: 200,
            resp_headers: Vec::new(),
            resp_body: None,
            kind: "fetch".into(),
            sent: Vec::new(),
            got: Vec::new(),
        }
    }

    fn posted(url: &str, body: Value) -> Exchange {
        let mut x = call(url);
        x.method = "POST".into();
        x.req_body = Some(Body { mime: "application/json".into(), json: Some(body), size: 0 });
        x
    }

    fn rows(n: usize) -> Value {
        Value::Array((0..n).map(|i| json!({"id": i})).collect())
    }

    #[test]
    fn a_cursor_parameter_gives_cursor_paging() {
        let a = call("https://x.test/api/items?limit=20");
        let b = call("https://x.test/api/items?limit=20&after=abc");
        let resp = Shape::of(&json!({"data": rows(3), "pageInfo": {"endCursor": "abc", "hasNextPage": true}}));
        let p = detect(&[&a, &b], &resp).expect("paging");
        assert_eq!(p.style, "cursor");
        assert_eq!(p.param.as_deref(), Some("after"));
        assert_eq!(p.size.as_deref(), Some("limit"));
        assert_eq!(p.next.as_deref(), Some("$.pageInfo.endCursor"));
        assert_eq!(p.items.as_deref(), Some("$.data"));
        assert_eq!(p.describe(), "cursor via ?after, next at $.pageInfo.endCursor, items at $.data, size ?limit");
    }

    #[test]
    fn cursor_names_are_compared_without_case_or_separators() {
        for name in ["pageToken", "next_page_token", "Continuation-Token", "startToken", "before", "CURSOR"] {
            let x = call(&format!("https://x.test/a?{name}=1"));
            let p = detect(&[&x], &Shape::of(&json!({"items": rows(2)}))).expect(name);
            assert_eq!(p.style, "cursor", "{name}");
            assert_eq!(p.param.as_deref(), Some(name));
        }
    }

    #[test]
    fn a_next_url_field_gives_next_url_paging() {
        let a = call("https://x.test/api/items");
        let mut resp = Shape::of(&json!({"results": rows(4), "next": "https://x.test/api/items?page=2"}));
        resp.add(&json!({"results": rows(1), "next": null}));
        let p = detect(&[&a], &resp).expect("paging");
        assert_eq!(p.style, "next url");
        assert_eq!(p.param, None);
        assert_eq!(p.next.as_deref(), Some("$.next"));
        assert_eq!(p.items.as_deref(), Some("$.results"));
        assert_eq!(p.describe(), "next url, next at $.next, items at $.results");
    }

    #[test]
    fn a_page_parameter_gives_page_paging() {
        let a = call("https://x.test/api/items?page=2&per_page=10");
        let p = detect(&[&a], &Shape::of(&json!({"items": rows(2)}))).expect("paging");
        assert_eq!(p.style, "page");
        assert_eq!(p.param.as_deref(), Some("page"));
        assert_eq!(p.size.as_deref(), Some("per_page"));
        assert_eq!(p.next, None);
        assert_eq!(p.items.as_deref(), Some("$.items"));
    }

    #[test]
    fn p_is_a_page_only_when_its_values_are_digits() {
        let ok = call("https://x.test/a?p=3");
        let resp = Shape::of(&json!({"items": rows(2)}));
        assert_eq!(detect(&[&ok], &resp).map(|p| p.style), Some("page"));
        let word = call("https://x.test/a?p=home");
        assert_eq!(detect(&[&word], &resp), None);
        let mixed1 = call("https://x.test/a?p=3");
        let mixed2 = call("https://x.test/a?p=x");
        assert_eq!(detect(&[&mixed1, &mixed2], &resp), None);
    }

    #[test]
    fn an_offset_parameter_gives_offset_paging() {
        for name in ["offset", "skip", "start", "from"] {
            let a = call(&format!("https://x.test/api/items?{name}=40&limit=20"));
            let p = detect(&[&a], &Shape::of(&json!(rows(5)))).expect(name);
            assert_eq!(p.style, "offset", "{name}");
            assert_eq!(p.param.as_deref(), Some(name));
            assert_eq!(p.size.as_deref(), Some("limit"));
            assert_eq!(p.items.as_deref(), Some("$"));
        }
    }

    #[test]
    fn a_link_header_with_rel_next_gives_link_header_paging() {
        let mut a = call("https://x.test/api/items");
        a.resp_headers.push((
            "link".into(),
            "<https://x.test/api/items?q=2>; rel=\"next\", <https://x.test/z>; rel=\"last\"".into(),
        ));
        let p = detect(&[&a], &Shape::of(&rows(3))).expect("paging");
        assert_eq!(p.style, "link header");
        assert_eq!(p.param, None);
        assert_eq!(p.items.as_deref(), Some("$"));
        assert_eq!(p.describe(), "link header (rel=next), items at $");
        let mut only_prev = call("https://x.test/api/items");
        only_prev.resp_headers.push(("link".into(), "<https://x.test/a>; rel=\"prev\"".into()));
        assert_eq!(detect(&[&only_prev], &Shape::of(&rows(3))), None);
    }

    #[test]
    fn a_next_field_alone_gives_cursor_paging_without_a_parameter() {
        let a = call("https://x.test/api/items");
        let resp = Shape::of(&json!({"items": rows(2), "next_cursor": "abc"}));
        let p = detect(&[&a], &resp).expect("paging");
        assert_eq!(p.style, "cursor");
        assert_eq!(p.param, None);
        assert_eq!(p.next.as_deref(), Some("$.next_cursor"));
        assert_eq!(p.describe(), "cursor, next at $.next_cursor, items at $.items");
    }

    #[test]
    fn graphql_variables_count_as_parameters() {
        let a = posted(
            "https://x.test/graphql",
            json!({"query": "query L($first: Int, $after: String) { l }", "variables": {"first": 10, "after": "c1"}}),
        );
        let resp = Shape::of(&json!({"data": {"pageInfo": {"endCursor": "c2"}, "edges": rows(3)}}));
        let p = detect(&[&a], &resp).expect("paging");
        assert_eq!(p.style, "cursor");
        assert_eq!(p.param.as_deref(), Some("variables.after"));
        assert_eq!(p.size.as_deref(), Some("variables.first"));
        assert_eq!(p.next.as_deref(), Some("$.data.pageInfo.endCursor"));
        assert_eq!(p.items.as_deref(), Some("$.data.edges"));
        assert!(p.describe().starts_with("cursor via variables.after, next at "));
    }

    #[test]
    fn fields_of_a_json_body_count_as_parameters() {
        let a = posted("https://x.test/api/list", json!({"cursor": "z", "limit": 5}));
        let p = detect(&[&a], &Shape::of(&json!({"rows": rows(2)}))).expect("paging");
        assert_eq!(p.param.as_deref(), Some("body.cursor"));
        assert_eq!(p.size.as_deref(), Some("body.limit"));
    }

    #[test]
    fn the_order_is_cursor_then_next_url_then_page_then_offset() {
        let resp_url = Shape::of(&json!({"items": rows(2), "next": "https://x.test/a?c=2"}));
        let cursor = call("https://x.test/a?after=1&page=2&offset=3");
        assert_eq!(detect(&[&cursor], &resp_url).map(|p| p.style), Some("cursor"));
        let page = call("https://x.test/a?page=2&offset=3");
        assert_eq!(detect(&[&page], &resp_url).map(|p| p.style), Some("next url"));
        let resp_plain = Shape::of(&json!({"items": rows(2)}));
        assert_eq!(detect(&[&page], &resp_plain).map(|p| p.style), Some("page"));
        let offset = call("https://x.test/a?offset=3");
        assert_eq!(detect(&[&offset], &resp_plain).map(|p| p.style), Some("offset"));
    }

    #[test]
    fn the_items_are_the_longest_array_and_a_top_level_array_is_dollar() {
        let a = call("https://x.test/a?page=1");
        let resp = Shape::of(&json!({"tags": ["a", "b"], "rows": rows(7), "other": rows(3)}));
        assert_eq!(detect(&[&a], &resp).and_then(|p| p.items).as_deref(), Some("$.rows"));
        let top = Shape::of(&rows(4));
        assert_eq!(detect(&[&a], &top).and_then(|p| p.items).as_deref(), Some("$"));
        let nested = Shape::of(&json!({"rows": [{"tags": [1, 2, 3, 4, 5, 6, 7, 8, 9]}, {"tags": []}]}));
        assert_eq!(detect(&[&a], &nested).and_then(|p| p.items).as_deref(), Some("$.rows"));
    }

    #[test]
    fn no_sign_of_paging_gives_none() {
        let a = call("https://x.test/api/user?id=4&q=hello");
        assert_eq!(detect(&[&a], &Shape::of(&json!({"name": "ann", "tags": ["a"]}))), None);
        assert_eq!(detect(&[], &Shape::default()), None);
        let b = call("https://x.test/api/x?limit=5");
        assert_eq!(detect(&[&b], &Shape::of(&json!({"name": "ann"}))), None);
    }

    #[test]
    fn an_opaque_cursor_value_never_reaches_the_description() {
        let token = "Zm9vYmFy_BazQux1234ABCDefgh-ij";
        assert!(opaque(token));
        let a = call(&format!("https://x.test/api?limit=5&after={token}"));
        let resp = Shape::of(&json!({"data": rows(2), "pageInfo": {"endCursor": token}}));
        let p = detect(&[&a], &resp).expect("paging");
        assert!(!p.describe().contains(token));
        assert!(!format!("{p:?}").contains(token));
        let secret = call("https://x.test/api?after=%3Csecret%3E");
        let q = detect(&[&secret], &resp).expect("paging");
        assert!(!format!("{q:?}").contains("secret"));
    }
}
