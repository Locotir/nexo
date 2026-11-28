use super::*;

/// What a transcript says about fetching cut results back.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub requests: usize,
    /// Cut results fetched back (a key once, however often it was asked for).
    pub fetched: BTreeSet<String>,
    pub recall_calls: usize,
    /// Prior calls (deduplicated by key) with their result out of view, summed over the requests.
    pub expo_out: u64,
    /// The same for results still in view.
    pub expo_in: u64,
    /// Calls identical to an earlier one whose result was still in view.
    pub repeats_in: u64,
    /// Whether the host offered `recall`.
    pub has_recall: bool,
}

fn is_edit(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "str_replace" | "patch" | "apply_patch" | "multi_edit" | "multiedit" | "create"
    )
}

/// The identity of a call: tool and arguments, a read by its path alone.
fn call_key(name: &str, input: &Value) -> String {
    if Tool::of(name) == Tool::Read {
        for k in ["path", "file_path", "file", "filename"] {
            if let Some(p) = input[k].as_str() {
                return format!("read:{p}");
            }
        }
    }
    format!("{}:{}", name.to_ascii_lowercase(), input)
}

/// Whether a result's text is a curator mark that lost content (not the lossless "a later output below").
pub fn lost_mark(text: &str) -> bool {
    text.contains("[curator]")
        && !text.contains("a later output below")
        && (text.contains("left out here") || text.contains("lines omitted") || text.contains("is shortened above"))
}

fn result_text(b: &Value) -> String {
    match &b["content"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

fn calls_in(m: &Value) -> Vec<(String, String, Value)> {
    let Some(bs) = m["content"].as_array() else { return Vec::new() };
    bs.iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            (
                b["id"].as_str().unwrap_or("").to_string(),
                b["name"].as_str().unwrap_or("").to_string(),
                b["input"].clone(),
            )
        })
        .collect()
}

/// Reads the induced re-fetches off a transcript (`c2-q-por-modelo.md`: a `recall` call, or a call
/// identical to an earlier one whose result was out of view at that request).
pub fn analyse(requests: &[Request]) -> Analysis {
    let mut a = Analysis { requests: requests.len(), ..Default::default() };
    a.has_recall = requests.iter().any(|q| q.tools.as_ref().is_some_and(|t| t.iter().any(|n| n == "recall")));
    for (i, q) in requests.iter().enumerate() {
        let Some(reply) = reply_to(requests, i) else { continue };
        // prior calls in this request: key -> result lost?
        let mut open: BTreeMap<String, String> = BTreeMap::new(); // id -> key
        let mut prior: BTreeMap<String, bool> = BTreeMap::new();
        for m in &q.messages {
            for (id, name, input) in calls_in(m) {
                if !is_edit(&name) && name != "recall" {
                    open.insert(id, call_key(&name, &input));
                }
            }
            if let Some(bs) = m["content"].as_array() {
                for b in bs.iter().filter(|b| b["type"] == "tool_result") {
                    if let Some(key) = b["tool_use_id"].as_str().and_then(|id| open.get(id)) {
                        prior.insert(key.clone(), lost_mark(&result_text(b)));
                    }
                }
            }
        }
        a.expo_out += prior.values().filter(|&&lost| lost).count() as u64;
        a.expo_in += prior.values().filter(|&&lost| !lost).count() as u64;
        for (_, name, input) in calls_in(reply) {
            if name == "recall" {
                a.recall_calls += 1;
                a.fetched.insert(format!("recall:{input}"));
            } else if is_edit(&name) {
                continue;
            } else {
                let key = call_key(&name, &input);
                match prior.get(&key) {
                    Some(true) => {
                        a.fetched.insert(key);
                    }
                    Some(false) => a.repeats_in += 1,
                    None => {}
                }
            }
        }
    }
    a
}

// ---- statistics ----
