use serde_json::Value;

/// A Jupyter notebook's cells as plain text: each cell's source, then its outputs; a picture or
/// other binary output becomes one line naming its type and size. `None` when it is not one.
pub(super) fn notebook(text: &str) -> Option<(usize, String)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let cells = v.get("cells")?.as_array()?;
    let joined = |v: Option<&Value>| -> String {
        let s = match v {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
            _ => String::new(),
        };
        if s.is_empty() || s.ends_with('\n') {
            s
        } else {
            s + "\n"
        }
    };
    let mut out = String::new();
    for (i, c) in cells.iter().enumerate() {
        let kind = c.get("cell_type").and_then(Value::as_str).unwrap_or("cell");
        out.push_str(&format!("[cell {} · {kind}]\n", i + 1));
        out.push_str(&joined(c.get("source")));
        let outputs = c.get("outputs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        for o in outputs {
            out.push_str("[output]\n");
            match o.get("output_type").and_then(Value::as_str) {
                Some("stream") => out.push_str(&joined(o.get("text"))),
                Some("error") => {
                    let field = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    out.push_str(&format!("{}: {}\n", field("ename"), field("evalue")));
                }
                _ => {
                    let Some(data) = o.get("data").and_then(Value::as_object) else { continue };
                    if let Some(t) = data.get("text/plain") {
                        out.push_str(&joined(Some(t)));
                    }
                    for (mime, body) in data.iter().filter(|(m, _)| *m != "text/plain") {
                        let size = joined(Some(body)).len();
                        out.push_str(&format!("({mime}, {} KiB, not shown)\n", size.div_ceil(1024)));
                    }
                }
            }
        }
    }
    Some((cells.len(), out))
}
