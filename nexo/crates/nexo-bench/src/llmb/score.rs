//! The distribution's two scorers: `exact_fields` (strict JSON equality per field) and
//! `swe_flux_fields` (SWE-Flux's comparison rules per field, see `flux`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::flux;

/// An instance's `oracle.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Oracle {
    #[serde(default)]
    pub comparison: Option<String>,
    pub expected: Map<String, Value>,
    pub scoreable_fields: Vec<String>,
}

impl Oracle {
    /// The scorer the oracle asks for.
    pub fn kind(&self) -> &'static str {
        if self.comparison.as_deref() == Some("swe_flux") {
            "swe_flux_fields"
        } else {
            "exact_fields"
        }
    }

    /// Scores `answer` (None: no answer could be read, every field wrong).
    pub fn score(&self, answer: Option<&Map<String, Value>>) -> Scored {
        let empty = Map::new();
        let a = answer.unwrap_or(&empty);
        if self.kind() == "swe_flux_fields" {
            swe_flux_fields(a, self)
        } else {
            exact_fields(a, &self.expected, &self.scoreable_fields)
        }
    }
}

/// One field's verdict.
#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub field: String,
    pub correct: bool,
    pub actual: Option<Value>,
    pub expected: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A scored answer.
#[derive(Debug, Clone, Serialize)]
pub struct Scored {
    pub kind: &'static str,
    pub correct: usize,
    pub total: usize,
    pub components: Vec<Component>,
    pub undeclared_fields: Vec<String>,
}

impl Scored {
    /// Correct fields over all fields (0 when there are none).
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.correct as f64 / self.total as f64
        }
    }

    pub fn all_correct(&self) -> bool {
        self.total > 0 && self.correct == self.total
    }
}

/// Equality with the types kept apart: `1` is not `1.0`, `true` is not `1`, a dict needs exactly
/// the same keys.
pub fn json_equal(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(e)) => {
            if a.is_f64() != e.is_f64() {
                false
            } else if e.is_f64() {
                a.as_f64() == e.as_f64()
            } else {
                a == e
            }
        }
        (Value::Object(a), Value::Object(e)) => {
            a.len() == e.len() && e.iter().all(|(k, v)| a.get(k).is_some_and(|w| json_equal(w, v)))
        }
        (Value::Array(a), Value::Array(e)) => a.len() == e.len() && a.iter().zip(e).all(|(x, y)| json_equal(x, y)),
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(e)) => a == e,
        (Value::String(a), Value::String(e)) => a == e,
        _ => false,
    }
}

fn undeclared(answer: &Map<String, Value>, fields: &[String]) -> Vec<String> {
    let mut u: Vec<String> = answer.keys().filter(|k| !fields.contains(k)).cloned().collect();
    u.sort();
    u
}

/// Each field right when the answer has it and it equals the expected value exactly.
pub fn exact_fields(answer: &Map<String, Value>, expected: &Map<String, Value>, fields: &[String]) -> Scored {
    let components: Vec<Component> = fields
        .iter()
        .map(|f| {
            let exp = expected.get(f).cloned().unwrap_or(Value::Null);
            Component {
                field: f.clone(),
                correct: answer.get(f).is_some_and(|a| json_equal(a, &exp)),
                actual: answer.get(f).cloned(),
                expected: exp,
                reason: None,
            }
        })
        .collect();
    Scored {
        kind: "exact_fields",
        correct: components.iter().filter(|c| c.correct).count(),
        total: fields.len(),
        components,
        undeclared_fields: undeclared(answer, fields),
    }
}

/// Each field right when SWE-Flux's rules find the answer's value to be the expected one.
pub fn swe_flux_fields(answer: &Map<String, Value>, oracle: &Oracle) -> Scored {
    let components: Vec<Component> = oracle
        .scoreable_fields
        .iter()
        .map(|f| {
            let exp = oracle.expected.get(f).cloned().unwrap_or(Value::Null);
            let (correct, reason) = match answer.get(f) {
                Some(a) => match flux::compare_values(&exp, a, f, &format!("$.{f}"), 1e-6) {
                    Ok(()) => (true, "ok".to_string()),
                    Err(why) => (false, why),
                },
                None => (false, "missing".to_string()),
            };
            Component { field: f.clone(), correct, actual: answer.get(f).cloned(), expected: exp, reason: Some(reason) }
        })
        .collect();
    Scored {
        kind: "swe_flux_fields",
        correct: components.iter().filter(|c| c.correct).count(),
        total: components.len(),
        components,
        undeclared_fields: undeclared(answer, &oracle.scoreable_fields),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn exact_fields_keep_types_apart() {
        assert!(json_equal(&json!({"a": [1, "x"]}), &json!({"a": [1, "x"]})));
        assert!(!json_equal(&json!(1.0), &json!(1)));
        assert!(!json_equal(&json!(true), &json!(1)));
        assert!(!json_equal(&json!({"a": 1, "b": 2}), &json!({"a": 1})));
        let fields = vec!["a".to_string(), "b".to_string()];
        let s = exact_fields(&map(json!({"a": 1, "z": 0})), &map(json!({"a": 1, "b": 2})), &fields);
        assert_eq!((s.correct, s.total, s.undeclared_fields.clone()), (1, 2, vec!["z".to_string()]));
        assert!(!s.all_correct() && (s.fraction() - 0.5).abs() < 1e-12);
    }

    #[test]
    fn the_oracle_picks_its_scorer() {
        let o: Oracle = serde_json::from_value(json!({
            "comparison": "swe_flux", "expected": {"n": 3, "f": "pkg/m"}, "scoreable_fields": ["n", "f"]
        }))
        .unwrap();
        assert_eq!(o.kind(), "swe_flux_fields");
        let s = o.score(Some(&map(json!({"n": "3", "f": "pkg.m"}))));
        assert!(s.all_correct(), "{s:?}");
        let s = o.score(None);
        assert_eq!((s.correct, s.components[0].reason.as_deref()), (0, Some("missing")));
        let e: Oracle = serde_json::from_value(json!({"expected": {"n": 3}, "scoreable_fields": ["n"]})).unwrap();
        assert_eq!(e.score(Some(&map(json!({"n": "3"})))).correct, 0);
    }
}
