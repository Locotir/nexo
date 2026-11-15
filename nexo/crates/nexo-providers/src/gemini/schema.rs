//! The tool schema without the keywords the API rejects.

use serde_json::{Map, Value};

/// The schema without the keywords the API rejects.
pub fn clean_schema(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, v) in o {
                // the children of `properties` are names chosen by the tool, not keywords
                if k == "properties" {
                    if let Value::Object(props) = v {
                        let kept = props.iter().map(|(n, s)| (n.clone(), clean_schema(s))).collect();
                        m.insert(k.clone(), Value::Object(kept));
                        continue;
                    }
                }
                if matches!(
                    k.as_str(),
                    "additionalProperties" | "$schema" | "$id" | "default" | "examples" | "const" | "patternProperties"
                ) {
                    continue;
                }
                m.insert(k.clone(), clean_schema(v));
            }
            Value::Object(m)
        }
        Value::Array(a) => Value::Array(a.iter().map(clean_schema).collect()),
        other => other.clone(),
    }
}
