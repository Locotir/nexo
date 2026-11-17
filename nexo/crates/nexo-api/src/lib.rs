//! nexo api: turns the traffic of a website (a HAR file, or what nexo's browser recorded) into a description of the
//! API behind it that an agent can read in a few lines and call without a browser: endpoints with their path
//! parameters, query, body and response shapes, how the calls authenticate, how lists page, GraphQL operations, as a
//! short summary or as OpenAPI 3.1. All of it is worked out in Rust from the traffic; no model reads the raw capture,
//! and no secret value survives the reading (see [`exchange`]).

pub mod auth;
pub mod call;
pub mod diff;
pub mod exchange;
pub mod graphql;
pub mod har;
pub mod noise;
pub mod openapi;
pub mod paging;
pub mod shape;
pub mod spec;
pub mod store;
pub mod summary;
pub mod template;

/// Reads the text of a HAR file and works out the API behind it: [`har::read`] and then [`spec::build`].
pub fn analyze_har(text: &str, o: &spec::Options) -> Result<spec::Api, String> {
    let all = har::read(text)?;
    Ok(spec::build(&all, o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scheme;
    use serde_json::{json, Value};

    const TOKEN: &str = "tkAAAA1111bbbbCCCC2222dddd";
    const SESSION: &str = "sessVALUE9876zzzzYYYY";
    const CSRF: &str = "csrfVALUE5555qqqqWWWW";
    const PASSWORD: &str = "hunter2hunter2";

    fn pairs(list: &[(&str, &str)]) -> Value {
        Value::Array(list.iter().map(|(n, v)| json!({ "name": n, "value": v })).collect())
    }

    /// One HAR entry: method, url, request headers, request body, status, response headers, response body, type.
    #[allow(clippy::too_many_arguments)]
    fn entry(
        method: &str,
        url: &str,
        req_headers: &[(&str, &str)],
        req_body: Option<&str>,
        status: u16,
        resp_headers: &[(&str, &str)],
        resp_body: Option<(&str, &str)>,
        kind: &str,
    ) -> Value {
        let mut request = json!({ "method": method, "url": url, "headers": pairs(req_headers) });
        if let Some(b) = req_body {
            request["postData"] = json!({ "mimeType": "application/json", "text": b });
        }
        let mut content = json!({});
        if let Some((mime, text)) = resp_body {
            content = json!({ "mimeType": mime, "text": text });
        }
        json!({
            "_resourceType": kind,
            "request": request,
            "response": { "status": status, "headers": pairs(resp_headers), "content": content },
        })
    }

    fn test_har() -> String {
        let auth = format!("Bearer {TOKEN}");
        let cookies = format!("csrftoken={CSRF}; sessionid={SESSION}; _ga=GA1.2.3333");
        let csrf_cookie = format!("csrftoken={CSRF}; Path=/");
        let session_cookie = format!("sessionid={SESSION}; Path=/; HttpOnly");
        let login = format!(r#"{{"email":"ann@example.com","password":"{PASSWORD}"}}"#);
        let login_resp = format!(r#"{{"access_token":"{TOKEN}","user":{{"id":1,"name":"Ann"}}}}"#);
        let gql = r#"{"operationName":"GetUser","query":"query GetUser($id: ID!) { user(id: $id) { id name } }","variables":{"id":"7"}}"#;
        let json = "application/json";
        let sent = [("authorization", auth.as_str()), ("cookie", cookies.as_str()), ("x-csrftoken", CSRF)];
        let items = r#"{"items":[{"id":1,"name":"a"},{"id":2,"name":"b"}],"total":20}"#;
        let entries = vec![
            entry(
                "GET",
                "https://app.test/",
                &[],
                None,
                200,
                &[("set-cookie", csrf_cookie.as_str())],
                Some(("text/html", "<html></html>")),
                "document",
            ),
            entry(
                "POST",
                "https://app.test/api/login",
                &[("cookie", format!("csrftoken={CSRF}").as_str())],
                Some(login.as_str()),
                200,
                &[("set-cookie", session_cookie.as_str())],
                Some((json, login_resp.as_str())),
                "xhr",
            ),
            entry(
                "GET",
                "https://app.test/api/items?page=1&limit=10",
                &sent,
                None,
                200,
                &[],
                Some((json, items)),
                "xhr",
            ),
            entry(
                "GET",
                "https://app.test/api/items?page=2&limit=10",
                &sent,
                None,
                200,
                &[],
                Some((json, items)),
                "xhr",
            ),
            entry(
                "GET",
                "https://app.test/api/items/42",
                &sent,
                None,
                200,
                &[],
                Some((json, r#"{"id":42,"name":"a"}"#)),
                "xhr",
            ),
            entry(
                "GET",
                "https://app.test/api/items/43",
                &sent,
                None,
                200,
                &[],
                Some((json, r#"{"id":43,"name":"b"}"#)),
                "xhr",
            ),
            entry(
                "POST",
                "https://app.test/graphql",
                &sent,
                Some(gql),
                200,
                &[],
                Some((json, r#"{"data":{"user":{"id":"7","name":"Ann"}}}"#)),
                "fetch",
            ),
            entry(
                "GET",
                "https://app.test/static/app.js",
                &[],
                None,
                200,
                &[],
                Some(("text/javascript", "var a=1;")),
                "script",
            ),
            entry("GET", "https://www.google-analytics.com/collect?v=1", &[], None, 200, &[], None, "ping"),
            entry("OPTIONS", "https://app.test/api/items", &[], None, 204, &[], None, "preflight"),
        ];
        json!({ "log": { "version": "1.2", "entries": entries } }).to_string()
    }

    fn api() -> spec::Api {
        analyze_har(&test_har(), &spec::Options::default()).unwrap()
    }

    #[test]
    fn a_har_without_entries_is_an_error() {
        assert!(analyze_har("{}", &spec::Options::default()).is_err());
        assert!(analyze_har("not json", &spec::Options::default()).is_err());
    }

    #[test]
    fn endpoints_are_grouped_and_their_paths_templated() {
        let api = api();
        assert_eq!(api.endpoints.len(), 4, "{:?}", api.endpoints.iter().map(|e| &e.id).collect::<Vec<_>>());
        let has = |m: &str, p: &str| api.endpoints.iter().any(|e| e.method == m && e.path == p);
        assert!(has("GET", "/api/items"));
        assert!(has("GET", "/api/items/{item_id}"));
        assert!(has("POST", "/api/login"));
        assert!(has("POST", "/graphql"));
        assert_eq!(api.total, 10);
        assert_eq!(api.hosts, vec!["https://app.test".to_string()]);
    }

    #[test]
    fn dropped_calls_are_counted_by_reason() {
        let api = api();
        assert_eq!(api.dropped.get("static file"), Some(&1));
        assert_eq!(api.dropped.get("tracker"), Some(&1));
        assert_eq!(api.dropped.get("preflight"), Some(&1));
        assert_eq!(api.dropped.get("page"), Some(&1));
    }

    #[test]
    fn the_bearer_token_is_traced_to_the_login_that_set_it() {
        let api = api();
        let bearer = api.auth.iter().find(|a| matches!(&a.scheme, Scheme::Bearer { .. })).expect("a bearer scheme");
        assert_eq!(bearer.scheme, Scheme::Bearer { header: "authorization".to_string() });
        assert_eq!(bearer.source.as_deref(), Some("set by POST /api/login ($.access_token)"));
        assert_eq!(bearer.uses, 5);
    }

    #[test]
    fn the_csrf_header_and_the_session_cookies_are_found() {
        let api = api();
        let csrf = api.auth.iter().find_map(|a| match &a.scheme {
            Scheme::Csrf { header, from } => Some((header.clone(), from.clone())),
            _ => None,
        });
        assert_eq!(csrf, Some(("x-csrftoken".to_string(), "cookie csrftoken".to_string())));
        let cookie = api.auth.iter().find_map(|a| match &a.scheme {
            Scheme::Cookie { names } => Some(names.clone()),
            _ => None,
        });
        let names = cookie.expect("a cookie scheme");
        assert!(names.contains(&"csrftoken".to_string()) && names.contains(&"sessionid".to_string()));
        assert!(!names.contains(&"_ga".to_string()), "analytics cookies are left out");
    }

    #[test]
    fn paging_and_graphql_are_found() {
        let api = api();
        let items = api.endpoints.iter().find(|e| e.path == "/api/items").unwrap();
        let p = items.paging.as_ref().expect("paging");
        assert_eq!(p.style, "page");
        assert_eq!(p.items.as_deref(), Some("$.items"));
        let g = api.endpoints.iter().find(|e| e.gql.is_some()).unwrap();
        assert_eq!(g.gql.as_deref(), Some("query GetUser"));
        assert_eq!(g.id, "gql_GetUser");
    }

    #[test]
    fn openapi_has_the_paths_the_schemes_and_the_graphql_operations() {
        let oa = openapi::to_openapi(&api(), "app.test API");
        assert_eq!(oa["openapi"], "3.1.0");
        assert!(oa["components"]["securitySchemes"].is_object());
        let schemes = oa["components"]["securitySchemes"].as_object().unwrap();
        assert!(schemes.values().any(|v| v["type"] == "http" && v["scheme"] == "bearer"));
        assert!(schemes.values().any(|v| v["type"] == "apiKey" && v["in"] == "cookie"));
        assert!(schemes.values().any(|v| v["x-nexo-from"] == "cookie csrftoken"));
        assert!(oa["paths"]["/api/items"]["get"].is_object());
        assert!(oa["paths"]["/api/items/{item_id}"]["get"]["parameters"][0]["required"] == true);
        assert!(oa["paths"]["/api/login"]["post"]["requestBody"].is_object());
        assert_eq!(oa["x-nexo-graphql"][0]["operation"], "GetUser");
        assert!(oa["paths"]["/api/items"]["get"]["security"].is_array());
    }

    #[test]
    fn the_summary_fits_its_budget_and_lists_what_matters() {
        let api = api();
        let s = summary::render(&api, 3000);
        assert!(s.chars().count() <= 3000, "{s}");
        assert!(s.contains("10 calls, 6 kept, 4 endpoints; dropped:"), "{s}");
        assert!(s.contains("bearer in authorization, set by POST /api/login ($.access_token)"), "{s}");
        assert!(s.contains("[get_api_items] GET /api/items x2"), "{s}");
        assert!(s.contains("paging: page via ?page"), "{s}");
        assert!(s.contains("query GetUser"), "{s}");
        let tight = summary::render(&api, 400);
        assert!(tight.contains("more endpoints (--json for all)"), "{tight}");
    }

    #[test]
    fn no_secret_reaches_the_summary_the_json_or_the_openapi() {
        let api = api();
        let oa = openapi::to_openapi(&api, "app.test API");
        let outs = [
            summary::render(&api, 6000),
            summary::render(&api, 300),
            serde_json::to_string(&oa).unwrap(),
            serde_json::to_string_pretty(&oa).unwrap(),
            format!("{api:?}"),
        ];
        for out in &outs {
            for secret in [TOKEN, SESSION, CSRF, PASSWORD] {
                assert!(!out.contains(secret), "{secret} leaked in {out}");
            }
        }
    }
}
