//! The canonical-step track: three frozen instances cut into independent objectives (6, 6 and 13),
//! one objective per turn of a single conversation, each scored field by field. A port of the
//! distribution's `battery`, `worker_request`, `split_components`, `json_statements` and
//! `score_snapshot`; the battery is checked against the manifest's frozen digests, so a port that
//! drifted from the original would refuse to run.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::score::{exact_fields, Scored};
use super::{answer, py, read_instance, sha};

/// The campaign file, relative to the distribution's root.
pub const MANIFEST: &str = "campaigns/canonical-step-v1.json";

/// The system prompt of the model under test, as the track gives it.
pub const WORKER_INSTRUCTIONS: &str = "You are the model under test in canonical-step.
Solve only the current independent objective, using the supplied frozen material.
You may read that material, and write and execute your own programs in your workspace.
Use the private venv and scratch directory. No downloads, outside material, other models,
or requests for hints. The supplied excerpts may not be directly executable: reconstructing
anything needed is your own work. Do not invent missing source or claim an experiment ran
when it did not. Give a compact answer; JSON is welcome but prose is acceptable.
Incomplete or uncertain answers and abstentions are acceptable. You need not fill gaps
by guessing. Later objectives belong to this same conversation, but only solve the current
one. Subsequent clarification requests are not feedback about correctness.
";

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub id: String,
    pub path: String,
    pub adapter: String,
    pub prompt_sha256: String,
    pub adapted_sha256: String,
    #[serde(default)]
    pub clarification: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    instances: Vec<Entry>,
}

/// One objective.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub request: String,
    pub components: Vec<String>,
    pub return_contract: String,
}

/// An instance cut into its objectives, with the material the model may read.
#[derive(Debug, Clone)]
pub struct Adapted {
    pub id: String,
    pub path: String,
    pub adapter: String,
    pub source_sha256: String,
    pub oracle_sha256: String,
    /// File name in `material/` → content.
    pub material: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

impl Adapted {
    /// The digest the manifest freezes: of the adapted instance as sorted, unescaped JSON.
    pub fn digest(&self) -> String {
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|s| {
                json!({"id": s.id, "request": s.request, "components": s.components,
                       "return_contract": s.return_contract})
            })
            .collect();
        let v = json!({
            "id": self.id, "path": self.path, "adapter": self.adapter,
            "source_sha256": self.source_sha256, "oracle_sha256": self.oracle_sha256,
            "material": self.material, "steps": steps,
        });
        sha::hex(py::dumps_sorted(&v).as_bytes())
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }
}

fn idx(text: &str, what: &str) -> Result<usize, String> {
    text.find(what).ok_or_else(|| format!("the prompt has no {what:?}"))
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

const PY_CONTRACT: &str = "Success: {\"status\":\"ok\",\"value\":...}; ValidationError: {\"status\":\"error\",\"messages\":...,\"valid_data\":...}. Omit inapplicable components. Render integer error indexes as JSON strings.";

/// Cuts `text` (the instance's prompt) into material and objectives as `entry.adapter` says.
pub fn adapt(entry: &Entry, text: &str, source_sha256: &str, oracle_sha256: &str) -> Result<Adapted, String> {
    let mut material = BTreeMap::new();
    let mut steps = Vec::new();
    let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match entry.adapter.as_str() {
        "python" => {
            static STEP: OnceLock<Regex> = OnceLock::new();
            let clar = entry.clarification.clone().ok_or("the python adapter needs a clarification")?;
            let source = &text[idx(text, "===== BEGIN")?..idx(text, "===== TASK =====")?];
            let task = text.split_once("===== TASK =====").map(|(_, t)| t).unwrap_or_default();
            let definitions = &task[..idx(task, "Evaluate these six independent cases.")?];
            material.insert("context.txt".to_string(), format!("{source}{definitions}\n{clar}\n"));
            for c in re(&STEP, r"(?s)### (\w+)\n```python\n(.*?)\n```").captures_iter(task) {
                steps.push(Step {
                    id: c[1].to_string(),
                    request: format!("{clar}\nInput:\n{}", &c[2]),
                    components: owned(&["status", "value", "messages", "valid_data"]),
                    return_contract: PY_CONTRACT.to_string(),
                });
            }
        }
        "cpp" => {
            static STEP: OnceLock<Regex> = OnceLock::new();
            let begin = idx(text, "===== BEGIN")?;
            let each = idx(text, "Each value must contain exactly:")?;
            let task = idx(text, "===== TASK =====")?;
            material.insert("context.txt".to_string(), format!("{}{}", &text[each..begin], &text[begin..task]));
            let contract = text[each..idx(text, "For every case,")?].trim().to_string();
            for c in re(&STEP, r"(?m)^\d+\. (\w+)\n(\{[^\n]+\})").captures_iter(text) {
                steps.push(Step {
                    id: c[1].to_string(),
                    request: format!("Construct Client::Impl, then call RetryGuard(func) exactly once.\n{}", &c[2]),
                    components: owned(&[
                        "connect_attempts",
                        "func_results",
                        "sleep_count",
                        "current_endpoint",
                        "outcome",
                    ]),
                    return_contract: contract.clone(),
                });
            }
        }
        "asm" => {
            static FUNCTION: OnceLock<Regex> = OnceLock::new();
            static CALL: OnceLock<Regex> = OnceLock::new();
            let preface = &text[..idx(text, "Return exactly one JSON object")?];
            let heads: Vec<(String, usize, usize)> = re(&FUNCTION, r"(?m)^Function ([ABC]):\n")
                .captures_iter(text)
                .map(|c| (c[1].to_string(), c.get(0).unwrap().start(), c.get(0).unwrap().end()))
                .collect();
            for (i, (letter, _, end)) in heads.iter().enumerate() {
                let stop = heads.get(i + 1).map_or(text.len(), |h| h.1);
                let section = &text[*end..stop];
                let (declaration, remainder) = section.split_once("Calls:\n").ok_or("a function has no calls")?;
                let (calls, listing) =
                    remainder.split_once(&format!("Assembly {letter}:\n")).ok_or("a function has no listing")?;
                material.insert(
                    format!("assembly-{letter}.txt"),
                    format!("{preface}Function {letter}:\n{declaration}Assembly {letter}:\n{listing}"),
                );
                let contract = if letter != "C" {
                    "Return std::vector<std::string> as an ordered array of strings."
                } else {
                    "Return boost::any with its preserved dynamic type (int, double, string) and value."
                };
                for c in re(&CALL, r"(?m)^- (\w+): (.+)$").captures_iter(calls) {
                    steps.push(Step {
                        id: c[1].to_string(),
                        request: format!("Function {letter}, call: {}\n{contract}", &c[2]),
                        components: if letter == "C" { owned(&["type", "value"]) } else { owned(&["return_value"]) },
                        return_contract: contract.to_string(),
                    });
                }
            }
        }
        other => return Err(format!("unknown adapter {other:?}")),
    }
    let want = match entry.adapter.as_str() {
        "python" | "cpp" => 6,
        _ => 13,
    };
    if steps.len() != want {
        return Err(format!("{}: the adapter recovered {} objectives, not {want}", entry.id, steps.len()));
    }
    Ok(Adapted {
        id: entry.id.clone(),
        path: entry.path.clone(),
        adapter: entry.adapter.clone(),
        source_sha256: source_sha256.to_string(),
        oracle_sha256: oracle_sha256.to_string(),
        material,
        steps,
    })
}

/// Every instance of the campaign under `root`, adapted and checked against the frozen digests.
pub fn battery(root: &Path) -> Result<Vec<Adapted>, String> {
    let path = root.join(MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for entry in &manifest.instances {
        let inst = read_instance(&root.join(&entry.path))?;
        if inst.prompt_sha256 != entry.prompt_sha256 {
            return Err(format!("{}: the source prompt changed", entry.id));
        }
        let adapted = adapt(entry, &inst.prompt, &inst.prompt_sha256, &inst.meta.sha256.oracle)?;
        if adapted.digest() != entry.adapted_sha256 {
            return Err(format!("{}: the adapted material differs from the frozen battery", entry.id));
        }
        out.push(adapted);
    }
    Ok(out)
}

/// The turn that asks for `step`; the first turn also says where the material is.
pub fn worker_request(inst: &Adapted, step: &Step, initial: bool) -> String {
    let mut s = String::new();
    if initial {
        let names: Vec<&str> = inst.material.keys().map(String::as_str).collect();
        s.push_str(&format!(
            "Frozen material is in material/: {}. Read as needed. Work only on this objective; do not solve future cases.\n",
            names.join(", ")
        ));
    }
    s.push_str(&format!("Current independent objective: {}\n{}\n", step.id, step.request));
    s.push_str(&format!("Requested output components: {}.\n", step.components.join(", ")));
    s.push_str(&step.return_contract);
    s.push_str("\nGive your result for this objective. An incomplete answer or abstention is acceptable.");
    s
}

/// The components of `value` that `step` asks for, with the status spellings of the python
/// adapter made canonical.
pub fn split_components(value: &Value, step: &Step) -> Map<String, Value> {
    let mut out = Map::new();
    if step.components == ["return_value"] {
        let v = value.get("return_value").filter(|_| value.is_object()).unwrap_or(value);
        out.insert("return_value".into(), v.clone());
        return out;
    }
    if let Value::Object(m) = value {
        for name in &step.components {
            if let Some(v) = m.get(name) {
                out.insert(name.clone(), v.clone());
            }
        }
    }
    if step.components.iter().any(|c| c == "valid_data") {
        if let Some(Value::String(s)) = out.get("status") {
            let canon = match s.to_lowercase().as_str() {
                "success" => Some("ok"),
                "validationerror" | "validation_error" => Some("error"),
                _ => None,
            };
            if let Some(c) = canon {
                out.insert("status".into(), Value::String(c.into()));
            }
        }
    }
    out
}

/// The statements a reply's JSON value makes, per objective: the value keyed by objective ids,
/// or the value of the current objective. None when it says something the contract has no place for.
pub fn json_statements(value: &Value, objective: &str, steps: &[Step]) -> Option<Vec<(String, Map<String, Value>)>> {
    let named = value.as_object().filter(|m| steps.iter().any(|s| m.contains_key(&s.id)));
    let values: Vec<(&Step, &Value)> = match named {
        Some(m) => {
            if m.keys().any(|k| !steps.iter().any(|s| &s.id == k)) {
                return None;
            }
            steps.iter().filter_map(|s| m.get(&s.id).map(|v| (s, v))).collect()
        }
        None => vec![(steps.iter().find(|s| s.id == objective)?, value)],
    };
    let mut out = Vec::new();
    for (step, answer) in values {
        if let Value::Object(a) = answer {
            if a.keys().any(|k| !step.components.contains(k)) {
                return None;
            }
        }
        let components = split_components(answer, step);
        if components.is_empty() && *answer != json!({}) {
            return None;
        }
        out.push((step.id.clone(), components));
    }
    Some(out)
}

/// What a reply states, read mechanically, in place of the track's model extractor. The whole
/// reply as JSON is read first; else the JSON in its ``` fences; else the JSON containers that
/// its prose encloses in no other container. Of several, the last that states something counts:
/// a model that corrects itself states its result last. A value is read strictly
/// (`json_statements`) first, then leniently: wrappers are descended (the children of an object
/// whose keys name neither an objective nor a component, the elements of an array) down to an
/// object keyed by objective ids or an object holding a component of the objective, and keys
/// beside the components are ignored. None: nothing readable, which the conversation asks about.
pub fn statements(text: &str, objective: &str, steps: &[Step]) -> Option<Vec<(String, Map<String, Value>)>> {
    if text.trim().is_empty() {
        return None;
    }
    if let Ok(v) = answer::plain_json(text) {
        return read_value(&v, objective, steps);
    }
    let mut candidates = answer::fenced_json(text);
    if candidates.is_empty() {
        candidates = answer::top_level_values(text);
    }
    candidates.iter().rev().find_map(|v| read_value(v, objective, steps))
}

/// How deep a wrapper may nest before the reader gives up on it.
const WRAP_DEPTH: usize = 8;

fn read_value(value: &Value, objective: &str, steps: &[Step]) -> Option<Vec<(String, Map<String, Value>)>> {
    json_statements(value, objective, steps).or_else(|| lenient(value, objective, steps, 0))
}

/// The lenient reading: an object keyed by objective ids states each one (keys naming no
/// objective are ignored); any other object states the current objective; an array is searched
/// from its last element.
fn lenient(value: &Value, objective: &str, steps: &[Step], depth: usize) -> Option<Vec<(String, Map<String, Value>)>> {
    if depth > WRAP_DEPTH {
        return None;
    }
    match value {
        Value::Object(m) if steps.iter().any(|s| m.contains_key(&s.id)) => {
            let out: Vec<(String, Map<String, Value>)> = steps
                .iter()
                .filter_map(|s| {
                    m.get(&s.id).and_then(|v| lenient_answer(v, s, steps, depth + 1)).map(|c| (s.id.clone(), c))
                })
                .collect();
            (!out.is_empty()).then_some(out)
        }
        Value::Object(_) => {
            let step = steps.iter().find(|s| s.id == objective)?;
            lenient_answer(value, step, steps, depth).map(|c| vec![(step.id.clone(), c)])
        }
        Value::Array(a) => a.iter().rev().find_map(|v| lenient(v, objective, steps, depth + 1)),
        _ => None,
    }
}

/// The components of `step` that `value`, or a wrapper inside it, holds.
fn lenient_answer(value: &Value, step: &Step, steps: &[Step], depth: usize) -> Option<Map<String, Value>> {
    if depth > WRAP_DEPTH {
        return None;
    }
    match value {
        Value::Object(m) => {
            if step.components.iter().any(|c| m.contains_key(c)) {
                let comps = split_components(value, step);
                return (!comps.is_empty()).then_some(comps);
            }
            m.iter()
                .filter(|(k, _)| !steps.iter().any(|s| &s.id == *k))
                .find_map(|(_, v)| lenient_answer(v, step, steps, depth + 1))
        }
        Value::Array(a) => a.iter().rev().find_map(|v| lenient_answer(v, step, steps, depth + 1)),
        _ => None,
    }
}

/// The score of a whole conversation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Snapshot {
    pub cases_correct: usize,
    pub total: usize,
    pub partial_percent: f64,
    pub cases: Vec<(String, Scored)>,
}

/// Scores the final statements of each objective against `expected` (the oracle's, by objective).
pub fn score_snapshot(
    inst: &Adapted,
    stated: &BTreeMap<String, Map<String, Value>>,
    expected: &Map<String, Value>,
) -> Snapshot {
    let empty = Map::new();
    let mut cases = Vec::new();
    for step in &inst.steps {
        let mut want = split_components(expected.get(&step.id).unwrap_or(&Value::Null), step);
        let mut actual = stated.get(&step.id).unwrap_or(&empty).clone();
        if inst.adapter == "asm" && step.id.starts_with("filter_") {
            let items = want.get("return_value").and_then(Value::as_array).cloned().unwrap_or_default();
            want = items.into_iter().enumerate().map(|(i, v)| (format!("element_{i}"), v)).collect();
            actual = match actual.get("return_value") {
                Some(Value::Array(r)) => r
                    .iter()
                    .take(2)
                    .enumerate()
                    .filter(|(i, _)| *i == 0 || r.len() == 2)
                    .map(|(i, v)| (format!("element_{i}"), v.clone()))
                    .collect(),
                _ => Map::new(),
            };
        }
        let fields: Vec<String> = want.keys().cloned().collect();
        cases.push((step.id.clone(), exact_fields(&actual, &want, &fields)));
    }
    let total = cases.len();
    let fractions: f64 = cases.iter().map(|(_, s)| s.fraction()).sum();
    Snapshot {
        cases_correct: cases.iter().filter(|(_, s)| s.correct == s.total).count(),
        total,
        partial_percent: if total == 0 { 0.0 } else { 100.0 * fractions / total as f64 },
        cases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str, comps: &[&str]) -> Step {
        Step {
            id: id.into(),
            request: String::new(),
            components: comps.iter().map(|s| s.to_string()).collect(),
            return_contract: String::new(),
        }
    }

    #[test]
    fn statements_name_objectives_or_answer_the_current_one() {
        let steps = vec![step("a", &["status", "value", "messages", "valid_data"]), step("b", &["return_value"])];
        let s = json_statements(&json!({"status": "Success", "value": 1}), "a", &steps).unwrap();
        assert_eq!(s, vec![("a".to_string(), json!({"status": "ok", "value": 1}).as_object().unwrap().clone())]);
        let s = json_statements(&json!({"b": ["x"]}), "a", &steps).unwrap();
        assert_eq!(s[0].1["return_value"], json!(["x"]));
        assert!(json_statements(&json!({"b": 1, "other": 2}), "a", &steps).is_none());
        assert!(json_statements(&json!({"status": "ok", "bogus": 1}), "a", &steps).is_none());
        assert!(json_statements(&json!(3), "a", &steps).is_none());
        let s = statements("Result:\n{\"return_value\": [\"x\"]}\ndone", "b", &steps).unwrap();
        assert_eq!(s[0].1["return_value"], json!(["x"]));
        assert!(statements("I abstain.", "b", &steps).is_none());
    }

    #[test]
    fn wrappers_fences_and_corrections_are_read_as_the_track_would() {
        let steps = vec![step("split_space_priority", &["return_value"]), step("any_int_double", &["type", "value"])];
        // a reply that wraps the result in the shape of its own working
        let wrapped = r#"{"objective": "split_space_priority", "calls": [{"call": "func0(\"alpha,beta gamma,,delta\")", "return_value": ["alpha", "beta gamma", "delta"]}], "notes": "split on spaces first"}"#;
        let s = statements(wrapped, "split_space_priority", &steps).unwrap();
        assert_eq!(
            s,
            vec![(
                "split_space_priority".to_string(),
                json!({"return_value": ["alpha", "beta gamma", "delta"]}).as_object().unwrap().clone()
            )]
        );
        // a bare array in a fence after prose
        let fenced = "Traced func0 end to end.\n\n```json\n[\"alpha,beta\", \"gamma,,delta\"]\n```";
        let s = statements(fenced, "split_space_priority", &steps).unwrap();
        assert_eq!(s[0].1["return_value"], json!(["alpha,beta", "gamma,,delta"]));
        // two fences: the last states the result
        let twice =
            "First guess:\n```json\n[\"a\"]\n```\nCorrected after reading the loop:\n```json\n[\"b\", \"c\"]\n```";
        assert_eq!(statements(twice, "split_space_priority", &steps).unwrap()[0].1["return_value"], json!(["b", "c"]));
        // the current objective's components inside prose, and under a return_value wrapper
        let s = statements("Result: {\"type\": \"double\", \"value\": 2.5}.", "any_int_double", &steps).unwrap();
        assert_eq!(s[0].1, json!({"type": "double", "value": 2.5}).as_object().unwrap().clone());
        let s = statements("{\"return_value\": {\"type\": \"int\", \"value\": 1}}", "any_int_double", &steps).unwrap();
        assert_eq!(s[0].1, json!({"type": "int", "value": 1}).as_object().unwrap().clone());
        // named objectives beside keys that name none
        let s = statements(
            "{\"objective\": \"both\", \"any_int_double\": {\"type\": \"double\"}}",
            "split_space_priority",
            &steps,
        )
        .unwrap();
        assert_eq!(s, vec![("any_int_double".to_string(), json!({"type": "double"}).as_object().unwrap().clone())]);
        // nothing the contract has a place for
        assert!(statements("{\"notes\": \"nothing to report\"}", "any_int_double", &steps).is_none());
        assert!(statements("I leave it unresolved.", "any_int_double", &steps).is_none());
    }

    #[test]
    fn filters_score_their_first_elements() {
        let inst = Adapted {
            id: "i".into(),
            path: String::new(),
            adapter: "asm".into(),
            source_sha256: String::new(),
            oracle_sha256: String::new(),
            material: BTreeMap::new(),
            steps: vec![step("filter_x", &["return_value"]), step("any_y", &["type", "value"])],
        };
        let expected = json!({"filter_x": ["abba", "True"], "any_y": {"type": "int", "value": 3}});
        let mut stated = BTreeMap::new();
        stated.insert(
            "filter_x".to_string(),
            json!({"return_value": ["abba", "True", "x"]}).as_object().unwrap().clone(),
        );
        stated.insert("any_y".to_string(), json!({"type": "int", "value": 3}).as_object().unwrap().clone());
        let s = score_snapshot(&inst, &stated, expected.as_object().unwrap());
        // three elements returned: only the first is read
        assert_eq!((s.cases[0].1.correct, s.cases[0].1.total), (1, 2));
        assert_eq!((s.cases_correct, s.total), (1, 2));
        assert!((s.partial_percent - 75.0).abs() < 1e-9);
    }
}
