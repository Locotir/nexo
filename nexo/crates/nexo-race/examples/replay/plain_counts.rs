use super::*;

/// The tokens plain mode sends over all requests, by kind: system and tools, user, assistant text,
/// assistant tool arguments, tool results. A message's tokens are shared among its blocks by size.
pub(super) fn plain_kinds(requests: &[Request]) -> [f64; 5] {
    let mut kinds = [0f64; 5];
    for r in requests {
        kinds[0] += r.system_tokens as f64;
        for m in &r.messages {
            let user = m["role"] == "user";
            let mut bytes = [0f64; 5];
            match &m["content"] {
                Value::Array(blocks) => {
                    for b in blocks {
                        let k = match b["type"].as_str() {
                            Some("tool_use") => 3,
                            Some("tool_result") => 4,
                            _ if user => 1,
                            _ => 2,
                        };
                        bytes[k] += b.to_string().len() as f64;
                    }
                }
                c => bytes[if user { 1 } else { 2 }] += c.to_string().len() as f64,
            }
            let all: f64 = bytes.iter().sum();
            if all > 0.0 {
                for (k, b) in bytes.iter().enumerate() {
                    kinds[k] += tokens_of(m) as f64 * b / all;
                }
            }
        }
    }
    kinds
}

/// The tokens of result lines in `msgs` that an earlier result in `msgs` already holds (lines of 8
/// characters or more, compared trimmed).
pub(super) fn repeated_tokens(msgs: &[Value]) -> i64 {
    let mut seen: HashSet<String> = HashSet::new();
    let mut n = 0;
    for m in msgs {
        let Some(Value::Array(blocks)) = m.get("content") else { continue };
        for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
            let text = match &b["content"] {
                Value::String(s) => s.clone(),
                Value::Array(inner) => inner.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
                _ => continue,
            };
            let mut mine = Vec::new();
            for line in text.lines().map(str::trim).filter(|l| l.len() >= 8) {
                if seen.contains(line) {
                    n += (line.len() as i64 + 3) / 4;
                } else {
                    mine.push(line.to_string());
                }
            }
            seen.extend(mine);
        }
    }
    n
}

/// How many messages of a list hold each distinctive word.
#[derive(Default)]
pub(super) struct Bag(HashMap<String, u32>);

impl Bag {
    pub(super) fn add(&mut self, words: &[String]) {
        for w in words {
            *self.0.entry(w.clone()).or_insert(0) += 1;
        }
    }

    pub(super) fn sub(&mut self, words: &[String]) {
        for w in words {
            if let Some(n) = self.0.get_mut(w) {
                *n -= 1;
                if *n == 0 {
                    self.0.remove(w);
                }
            }
        }
    }

    pub(super) fn has(&self, w: &str) -> bool {
        self.0.contains_key(w)
    }
}
