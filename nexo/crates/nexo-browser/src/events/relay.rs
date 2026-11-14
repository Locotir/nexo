//! Fills the buffers from raw protocol events, as they come over the relay from the user's own browser.
//! (The headless backend gets the same events typed, through `chromiumoxide`; see `Buffers::watch`.)

use super::{push, Buffers, ConsoleEntry, NetEntry};
use serde_json::Value;

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// What `console.log(arg)` shows for one argument: its value as text, or the description of an object.
fn show(arg: &Value) -> String {
    match arg.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => arg
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| arg.get("unserializableValue").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
    }
}

impl Buffers {
    /// Takes one event (`Runtime.consoleAPICalled`, `Runtime.exceptionThrown`, `Log.entryAdded`,
    /// `Network.requestWillBeSent`, `Network.responseReceived`, `Network.loadingFinished`, `Network.loadingFailed`)
    /// from this tab; any other is ignored. Returns whether it was one. The extension sends no headers; URLs are
    /// redacted here as on the headless browser.
    pub(crate) fn ingest(&self, tab: i64, method: &str, p: &Value) -> bool {
        let ts = |v: &Value| v.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        match method {
            "Runtime.consoleAPICalled" => {
                let args = p.get("args").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                let level = match text(p, "type").as_str() {
                    "warn" => "warning".to_string(),
                    other => other.to_string(),
                };
                push(
                    &self.console,
                    ConsoleEntry { level, text: args.iter().map(show).collect::<Vec<_>>().join(" "), ts: ts(p) },
                );
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let described = d.get("exception").map(|x| text(x, "description")).filter(|t| !t.is_empty());
                push(
                    &self.console,
                    ConsoleEntry {
                        level: "error".into(),
                        text: described.unwrap_or_else(|| text(d, "text")),
                        ts: ts(p),
                    },
                );
            }
            "Log.entryAdded" => {
                let e = &p["entry"];
                push(&self.console, ConsoleEntry { level: text(e, "level"), text: text(e, "text"), ts: ts(e) });
            }
            "Network.requestWillBeSent" => {
                let id = text(p, "requestId");
                let now = p.get("timestamp").and_then(Value::as_f64);
                if let Some(r) = p.get("redirectResponse") {
                    let status = r.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok());
                    self.answered(&id, status, &text(r, "statusText"), &text(r, "mimeType"), &r["headers"]);
                    self.ended(&id, now, None);
                }
                let r = &p["request"];
                let wall = p.get("wallTime").and_then(Value::as_f64).unwrap_or(0.0) * 1_000.0;
                let mut entry =
                    NetEntry::new(self.next_seq(), text(r, "method"), &text(r, "url"), wall, tab.to_string(), id);
                entry.request_headers = super::redact::header_list(&r["headers"]);
                entry.started = now;
                push(&self.net, entry);
            }
            "Network.responseReceived" => {
                let r = &p["response"];
                let status = r.get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok());
                self.answered(
                    &text(p, "requestId"),
                    status,
                    &text(r, "statusText"),
                    &text(r, "mimeType"),
                    &r["headers"],
                );
            }
            "Network.loadingFinished" => {
                self.ended(&text(p, "requestId"), p.get("timestamp").and_then(Value::as_f64), None)
            }
            "Network.loadingFailed" => {
                let why = Some(text(p, "errorText")).filter(|t| !t.is_empty()).or_else(|| Some("failed".into()));
                self.ended(&text(p, "requestId"), p.get("timestamp").and_then(Value::as_f64), why);
            }
            "Page.javascriptDialogOpening" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64() * 1_000.0);
                let shown = format!("{} dialog: {}", text(p, "type"), text(p, "message"));
                push(&self.console, ConsoleEntry { level: "dialog".into(), text: shown, ts: now });
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_events_fill_the_same_buffers() {
        let b = Buffers::default();
        assert!(b.ingest(
            7,
            "Runtime.consoleAPICalled",
            &json!({"type": "warn", "timestamp": 5.0, "args": [{"type": "string", "value": "hi"}, {"type": "number", "value": 3}, {"type": "object", "description": "Array(2)"}]})
        ));
        assert!(b.ingest(7, "Runtime.exceptionThrown", &json!({"timestamp": 6.0, "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: boom\n at x"}}})));
        assert!(b.ingest(
            7,
            "Log.entryAdded",
            &json!({"entry": {"level": "info", "text": "from log", "timestamp": 7.0}})
        ));
        let console = b.console(None).unwrap();
        assert_eq!(console.len(), 3);
        assert_eq!((console[0].level.as_str(), console[0].text.as_str()), ("warning", "hi 3 Array(2)"));
        assert_eq!(console[1].text, "Error: boom\n at x");
        assert_eq!(b.console(Some("^info$")).unwrap().len(), 1);

        assert!(b.ingest(
            7,
            "Network.requestWillBeSent",
            &json!({"requestId": "1", "wallTime": 2.0, "request": {"method": "GET", "url": "https://a.b/x"}})
        ));
        assert!(b.ingest(7, "Network.requestWillBeSent", &json!({"requestId": "1", "wallTime": 2.5, "request": {"method": "GET", "url": "https://a.b/y"}, "redirectResponse": {"status": 302}})));
        assert!(b.ingest(7, "Network.responseReceived", &json!({"requestId": "1", "response": {"status": 200}})));
        let net = b.network(None).unwrap();
        assert_eq!(net.len(), 2);
        assert_eq!((net[0].status, net[1].status), (Some(302), Some(200)));
        assert_eq!(net[1].ts, 2_500.0);
        assert_eq!((net[0].seq, net[1].seq, net[1].source.as_str()), (1, 2, "7"));
        // what the person's own browser sends is redacted as well, should it ever carry credentials
        assert!(b.ingest(
            7,
            "Network.requestWillBeSent",
            &json!({"requestId": "2", "wallTime": 3.0, "timestamp": 1.0, "request": {"method": "POST", "url": "https://me:pw@a.b/cb?token=t#access_token=a", "headers": {"Authorization": "Bearer x", "Accept": "*/*"}}})
        ));
        assert!(b.ingest(7, "Network.responseReceived", &json!({"requestId": "2", "response": {"status": 500, "statusText": "Oops", "mimeType": "text/plain", "headers": {"Set-Cookie": "sid=y"}}})));
        assert!(b.ingest(
            7,
            "Network.loadingFailed",
            &json!({"requestId": "2", "timestamp": 1.25, "errorText": "net::ERR_ABORTED"})
        ));
        let e = b.request(3).unwrap();
        assert_eq!(e.url, "https://me:REDACTED@a.b/cb?token=REDACTED#access_token=REDACTED");
        assert!(e.request_headers.contains(&("Authorization".into(), "REDACTED".into())), "{:?}", e.request_headers);
        assert!(e.request_headers.contains(&("Accept".into(), "*/*".into())), "{:?}", e.request_headers);
        assert_eq!(e.response_headers, [("Set-Cookie".to_string(), "REDACTED".to_string())]);
        assert_eq!(
            (e.status, e.status_text.as_str(), e.duration_ms, e.error.as_deref()),
            (Some(500), "Oops", Some(250.0), Some("net::ERR_ABORTED"))
        );
        assert!(!b.ingest(7, "Page.loadEventFired", &json!({})));
    }
}
