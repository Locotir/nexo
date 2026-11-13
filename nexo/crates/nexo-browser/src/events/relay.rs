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
    /// `Network.requestWillBeSent`, `Network.responseReceived`); any other is ignored. Returns whether it was one.
    pub(crate) fn ingest(&self, method: &str, p: &Value) -> bool {
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
                if let Some(status) = p.get("redirectResponse").and_then(|r| r.get("status")).and_then(Value::as_u64) {
                    if let Some(prev) =
                        super::lock(&self.net).iter_mut().rev().find(|n| n.id == id && n.status.is_none())
                    {
                        prev.status = u16::try_from(status).ok();
                    }
                }
                let r = &p["request"];
                let entry = NetEntry {
                    method: text(r, "method"),
                    url: text(r, "url"),
                    status: None,
                    ts: p.get("wallTime").and_then(Value::as_f64).unwrap_or(0.0) * 1_000.0,
                    id,
                };
                push(&self.net, entry);
            }
            "Network.responseReceived" => {
                let id = text(p, "requestId");
                let status = p["response"].get("status").and_then(Value::as_u64).and_then(|s| u16::try_from(s).ok());
                if let Some(hit) = super::lock(&self.net).iter_mut().rev().find(|n| n.id == id && n.status.is_none()) {
                    hit.status = status;
                }
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
            "Runtime.consoleAPICalled",
            &json!({"type": "warn", "timestamp": 5.0, "args": [{"type": "string", "value": "hi"}, {"type": "number", "value": 3}, {"type": "object", "description": "Array(2)"}]})
        ));
        assert!(b.ingest("Runtime.exceptionThrown", &json!({"timestamp": 6.0, "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: boom\n at x"}}})));
        assert!(b.ingest("Log.entryAdded", &json!({"entry": {"level": "info", "text": "from log", "timestamp": 7.0}})));
        let console = b.console(None).unwrap();
        assert_eq!(console.len(), 3);
        assert_eq!((console[0].level.as_str(), console[0].text.as_str()), ("warning", "hi 3 Array(2)"));
        assert_eq!(console[1].text, "Error: boom\n at x");
        assert_eq!(b.console(Some("^info$")).unwrap().len(), 1);

        assert!(b.ingest(
            "Network.requestWillBeSent",
            &json!({"requestId": "1", "wallTime": 2.0, "request": {"method": "GET", "url": "https://a.b/x"}})
        ));
        assert!(b.ingest("Network.requestWillBeSent", &json!({"requestId": "1", "wallTime": 2.5, "request": {"method": "GET", "url": "https://a.b/y"}, "redirectResponse": {"status": 302}})));
        assert!(b.ingest("Network.responseReceived", &json!({"requestId": "1", "response": {"status": 200}})));
        let net = b.network(None).unwrap();
        assert_eq!(net.len(), 2);
        assert_eq!((net[0].status, net[1].status), (Some(302), Some(200)));
        assert_eq!(net[1].ts, 2_500.0);
        assert!(!b.ingest("Page.loadEventFired", &json!({})));
    }
}
