//! Reads a HAR 1.2 file (Chrome and Edge DevTools, Firefox, Playwright `recordHar`, mitmproxy exports) into
//! [`Exchange`]s. Every entry goes through [`Raw::into_exchange`], so no secret value leaves this module.

use crate::exchange::{Exchange, Raw};
use serde_json::Value;

/// Reads the text of a HAR file. Entries keep their file order; an entry whose URL does not parse is skipped, and
/// `seq` counts only the entries that were kept. A file without `log.entries` is an error.
pub fn read(text: &str) -> Result<Vec<Exchange>, String> {
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("not a HAR file: the text is not valid JSON ({e})"))?;
    let entries = root
        .get("log")
        .and_then(|l| l.get("entries"))
        .and_then(Value::as_array)
        .ok_or_else(|| "not a HAR file: there is no log.entries list".to_string())?;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Some(raw) = raw_of(entry) {
            if let Some(x) = raw.into_exchange(out.len()) {
                out.push(x);
            }
        }
    }
    Ok(out)
}

/// A string field, empty when missing.
fn text_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `[{name, value}]` as pairs.
fn pairs_of(v: Option<&Value>) -> Vec<(String, String)> {
    let Some(list) = v.and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|h| {
            let name = h.get("name")?.as_str()?;
            let value = h.get("value").and_then(Value::as_str).unwrap_or("");
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// One HAR entry as a [`Raw`]; `None` when it has no request.
fn raw_of(entry: &Value) -> Option<Raw> {
    let req = entry.get("request")?;
    let mut raw = Raw {
        method: text_of(req, "method").to_string(),
        url: text_of(req, "url").to_string(),
        req_headers: pairs_of(req.get("headers")),
        kind: text_of(entry, "_resourceType").to_ascii_lowercase(),
        ..Raw::default()
    };

    if let Some(post) = req.get("postData") {
        let mime = text_of(post, "mimeType").to_string();
        let body = text_of(post, "text");
        if !body.is_empty() {
            raw.req_body = Some((mime, body.to_string()));
        } else {
            let params = pairs_of(post.get("params"));
            if !params.is_empty() {
                let mut form = url::form_urlencoded::Serializer::new(String::new());
                for (k, v) in &params {
                    form.append_pair(k, v);
                }
                let mime = if mime.is_empty() { "application/x-www-form-urlencoded".to_string() } else { mime };
                raw.req_body = Some((mime, form.finish()));
            }
        }
    }

    if let Some(resp) = entry.get("response") {
        raw.status = resp.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok()).unwrap_or(0);
        raw.resp_headers = pairs_of(resp.get("headers"));
        if let Some(content) = resp.get("content") {
            let mime = text_of(content, "mimeType").to_string();
            let body = text_of(content, "text");
            if !body.is_empty() {
                if text_of(content, "encoding").eq_ignore_ascii_case("base64") {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT.decode(body.trim()).ok();
                    if let Some(decoded) = bytes.and_then(|b| String::from_utf8(b).ok()) {
                        raw.resp_body = Some((mime, decoded));
                    }
                } else {
                    raw.resp_body = Some((mime, body.to_string()));
                }
            }
        }
    }
    Some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> String {
        use base64::Engine;
        let b64 =
            base64::engine::general_purpose::STANDARD.encode(r#"{"items":[1,2],"access_token":"b64-token-secret"}"#);
        let invalid = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xfe, 0xfd]);
        json!({"log": {"version": "1.2", "entries": [
            {
                "_resourceType": "XHR",
                "request": {
                    "method": "get",
                    "url": "https://api.test/v1/items?api_key=QUERYSECRET1234&page=1",
                    "headers": [
                        {"name": ":authority", "value": "api.test"},
                        {"name": "Authorization", "value": "Bearer HEADERSECRET1234"},
                        {"name": "Accept", "value": "application/json"}
                    ]
                },
                "response": {
                    "status": 200,
                    "headers": [{"name": "Content-Type", "value": "application/json"},
                                {"name": "Set-Cookie", "value": "sid=COOKIESECRET1234; Path=/"}],
                    "content": {"mimeType": "application/json", "text": b64, "encoding": "base64"}
                }
            },
            {
                "request": {"method": "GET", "url": "not a url", "headers": []},
                "response": {"status": 200, "headers": [], "content": {"mimeType": "text/plain", "text": "x"}}
            },
            {
                "_resourceType": "fetch",
                "request": {
                    "method": "POST",
                    "url": "https://api.test/login",
                    "headers": [],
                    "postData": {"mimeType": "application/x-www-form-urlencoded",
                                 "params": [{"name": "user", "value": "ann"}, {"name": "password", "value": "FORMSECRET1234"}]}
                },
                "response": {"status": 302, "headers": [], "content": {"mimeType": "text/html", "size": 0}}
            },
            {
                "request": {"method": "GET", "url": "https://api.test/blob", "headers": []},
                "response": {"status": 200, "headers": [], "content": {"mimeType": "application/octet-stream", "text": invalid, "encoding": "base64"}}
            }
        ]}})
        .to_string()
    }

    #[test]
    fn chrome_style_har_is_read_in_order_and_the_bad_url_is_skipped() {
        let xs = read(&sample()).unwrap();
        assert_eq!(xs.len(), 3);
        assert_eq!(xs.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(xs[0].method, "GET");
        assert_eq!(xs[0].kind, "xhr");
        assert_eq!(xs[0].status, 200);
        assert_eq!(xs[1].method, "POST");
        assert_eq!(xs[1].kind, "fetch");
        assert_eq!(xs[1].status, 302);
        assert_eq!(xs[2].url.path(), "/blob");
        assert_eq!(xs[2].kind, "");
    }

    #[test]
    fn headers_are_cleaned_and_pseudo_headers_dropped() {
        let xs = read(&sample()).unwrap();
        assert_eq!(xs[0].req_header("authorization"), Some("Bearer <secret>"));
        assert_eq!(xs[0].req_header(":authority"), None);
        assert_eq!(xs[0].req_header("accept"), Some("application/json"));
        assert_eq!(xs[0].resp_header("set-cookie"), Some("sid=<secret>; Path=/"));
        assert_eq!(xs[0].sent.len(), 2);
        assert_eq!(xs[0].got.len(), 2);
    }

    #[test]
    fn base64_body_is_decoded_and_parsed() {
        let xs = read(&sample()).unwrap();
        let b = xs[0].resp_body.as_ref().unwrap();
        assert_eq!(b.mime, "application/json");
        let j = b.json.as_ref().unwrap();
        assert_eq!(j["items"][1], 2);
        assert_eq!(j["access_token"], "<secret>");
    }

    #[test]
    fn base64_that_is_not_utf8_gives_no_body() {
        let xs = read(&sample()).unwrap();
        assert!(xs[2].resp_body.is_none());
    }

    #[test]
    fn form_params_become_a_form_body_with_secrets_replaced() {
        let xs = read(&sample()).unwrap();
        let b = xs[1].req_body.as_ref().unwrap();
        assert_eq!(b.mime, "application/x-www-form-urlencoded");
        let j = b.json.as_ref().unwrap();
        assert_eq!(j["user"], "ann");
        assert_eq!(j["password"], "<secret>");
        assert_eq!(b.size, "user=ann&password=FORMSECRET1234".len());
        assert!(xs[1].resp_body.is_none());
    }

    #[test]
    fn query_secret_is_replaced_in_the_url() {
        let xs = read(&sample()).unwrap();
        assert!(!xs[0].url.as_str().contains("QUERYSECRET1234"));
        assert!(xs[0].url.query_pairs().any(|(k, v)| k == "page" && v == "1"));
    }

    #[test]
    fn no_raw_secret_of_the_har_reaches_the_debug_output() {
        let xs = read(&sample()).unwrap();
        let dump = format!("{:?}", xs);
        for s in ["QUERYSECRET1234", "HEADERSECRET1234", "COOKIESECRET1234", "FORMSECRET1234", "b64-token-secret"] {
            assert!(!dump.contains(s), "{s} leaked");
        }
    }

    #[test]
    fn har_without_entries_is_an_error_with_a_plain_message() {
        let e = read(r#"{"log":{"version":"1.2"}}"#).unwrap_err();
        assert!(e.contains("log.entries"), "{e}");
        assert!(read(r#"{"nope":1}"#).is_err());
        assert!(read("this is not json").is_err());
    }

    #[test]
    fn empty_entry_list_is_fine() {
        assert!(read(r#"{"log":{"entries":[]}}"#).unwrap().is_empty());
    }

    #[test]
    fn byte_order_mark_is_ignored() {
        let text = format!("\u{feff}{}", r#"{"log":{"entries":[]}}"#);
        assert!(read(&text).is_ok());
    }
}
