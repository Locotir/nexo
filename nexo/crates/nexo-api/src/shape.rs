//! What JSON looks like, learned from many samples of it. A [`Shape`] counts what was seen at one place of a document
//! (types, string formats, a few enum values, array sizes, which object fields appear and how often) and merges any
//! number of samples into it. It is turned into a JSON Schema ([`Shape::schema`]) or a one-line sketch
//! ([`Shape::sketch`]) an agent can read.
//!
//! A shape never keeps a secret: it only keeps an example of a string that is not opaque (see
//! [`crate::exchange::opaque`]), a value that is already [`SECRET`] stays that way, and a field name that looks like a
//! token is shown as `<opaque>`.

use crate::exchange::{opaque, SECRET};
use indexmap::IndexMap;
use serde_json::{Map, Value};

/// How deep a document is read; what lies below is counted in [`Shape::cut`].
const MAX_DEPTH: usize = 12;
/// How many items of one array are read.
const MAX_ITEMS: usize = 50;
/// How many fields of one object are read.
const MAX_FIELDS: usize = 200;
/// Distinct short strings kept; holding this many means "more than 8".
const MAX_ENUM: usize = 9;
/// An object with at least this many keys may be a dictionary.
const DICT_MIN: usize = 8;
/// The longest example, in characters.
const EXAMPLE_CAP: usize = 60;
/// How many fields of one object a sketch lists.
const SKETCH_FIELDS: usize = 12;
/// How deep a sketch goes at most, and how deep [`Shape::walk`] goes.
const SKETCH_DEPTH: usize = 4;
/// What an example shows instead of an opaque string.
const OPAQUE_MARK: &str = "<opaque>";

/// The format all the strings of a shape share.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Fmt {
    /// No string seen yet.
    #[default]
    Unset,
    /// Strings of different formats, or of none.
    Mixed,
    Uuid,
    DateTime,
    Date,
    Email,
    Uri,
    Digits,
}

impl Fmt {
    /// Two formats seen at one place: unset gives way, equal stays, anything else is mixed.
    pub fn merge(self, other: Fmt) -> Fmt {
        match (self, other) {
            (Fmt::Unset, x) | (x, Fmt::Unset) => x,
            (a, b) if a == b => a,
            _ => Fmt::Mixed,
        }
    }

    /// The JSON Schema `format`, for the formats that have one.
    fn schema_format(self) -> Option<&'static str> {
        match self {
            Fmt::Uuid => Some("uuid"),
            Fmt::DateTime => Some("date-time"),
            Fmt::Date => Some("date"),
            Fmt::Email => Some("email"),
            Fmt::Uri => Some("uri"),
            _ => None,
        }
    }
}

/// Everything seen at one place of a JSON document, over all samples merged in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shape {
    /// How many values were merged in here (a field's `seen` is how many objects had it).
    pub seen: usize,
    pub nulls: usize,
    pub bools: usize,
    /// Whole numbers.
    pub ints: usize,
    /// Numbers with a fraction or an exponent.
    pub nums: usize,
    pub strs: usize,
    /// The format common to all the strings.
    pub fmt: Fmt,
    /// Up to 9 distinct short strings (32 characters or less), for enums. A string that is opaque, is [`SECRET`] or
    /// is longer never goes in.
    pub enums: Vec<String>,
    /// Whether a string was seen that cannot be an enum value: a long, opaque or secret one.
    pub long: bool,
    /// One safe example of a scalar value.
    pub example: Option<Value>,
    pub arrays: usize,
    /// What the items of all arrays look like; `None` when only empty arrays were seen.
    pub items: Option<Box<Shape>>,
    /// The longest array seen, in items.
    pub max_items: usize,
    pub objects: usize,
    /// The fields of the objects, in the order first seen.
    pub fields: IndexMap<String, Shape>,
    /// What the values look like when an object is a dictionary keyed by ids.
    pub dict: Option<Box<Shape>>,
    /// How many items, fields or levels were left unread because of the caps.
    pub cut: usize,
}

impl Shape {
    /// The shape of one value.
    pub fn of(v: &Value) -> Shape {
        let mut s = Shape::default();
        s.add(v);
        s
    }

    /// Merges one more sample in.
    pub fn add(&mut self, v: &Value) {
        self.add_at(v, 0);
    }

    fn add_at(&mut self, v: &Value, depth: usize) {
        self.seen += 1;
        match v {
            Value::Null => self.nulls += 1,
            Value::Bool(_) => {
                self.bools += 1;
                self.note_example(v.clone());
            }
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    self.ints += 1;
                } else {
                    self.nums += 1;
                }
                self.note_example(v.clone());
            }
            Value::String(s) => self.add_str(s),
            Value::Array(a) => {
                self.arrays += 1;
                self.max_items = self.max_items.max(a.len());
                if depth >= MAX_DEPTH {
                    self.cut += a.len();
                    return;
                }
                if a.is_empty() {
                    return;
                }
                let items = self.items.get_or_insert_with(Default::default);
                for x in a.iter().take(MAX_ITEMS) {
                    items.add_at(x, depth + 1);
                }
                self.cut += a.len().saturating_sub(MAX_ITEMS);
            }
            Value::Object(m) => {
                self.objects += 1;
                if depth >= MAX_DEPTH {
                    self.cut += m.len();
                    return;
                }
                if is_dict(m) {
                    let dict = self.dict.get_or_insert_with(Default::default);
                    for x in m.values().take(MAX_FIELDS) {
                        dict.add_at(x, depth + 1);
                    }
                    self.cut += m.len().saturating_sub(MAX_FIELDS);
                    return;
                }
                for (i, (k, x)) in m.iter().enumerate() {
                    if i >= MAX_FIELDS {
                        self.cut += m.len() - i;
                        break;
                    }
                    let key = if opaque(k) { OPAQUE_MARK.to_string() } else { k.clone() };
                    if !self.fields.contains_key(&key) && self.fields.len() >= MAX_FIELDS {
                        self.cut += 1;
                        continue;
                    }
                    self.fields.entry(key).or_default().add_at(x, depth + 1);
                }
            }
        }
    }

    fn add_str(&mut self, s: &str) {
        self.strs += 1;
        self.fmt = self.fmt.merge(fmt_of(s));
        if s == SECRET {
            self.long = true;
            self.note_example(Value::String(SECRET.to_string()));
        } else if opaque(s) {
            self.long = true;
            self.note_example(Value::String(OPAQUE_MARK.to_string()));
        } else {
            if s.chars().count() <= 32 {
                if self.enums.len() < MAX_ENUM && !self.enums.iter().any(|e| e == s) {
                    self.enums.push(s.to_string());
                }
            } else {
                self.long = true;
            }
            self.note_example(Value::String(cut_example(s)));
        }
    }

    /// Keeps the first example; a placeholder for an opaque string gives way to a real one.
    fn note_example(&mut self, v: Value) {
        let replace = match &self.example {
            None => true,
            Some(Value::String(s)) => s == OPAQUE_MARK && v != Value::String(OPAQUE_MARK.to_string()),
            Some(_) => false,
        };
        if replace {
            self.example = Some(v);
        }
    }

    /// Whether every object merged in here had this field.
    pub fn required(&self, field: &str) -> bool {
        self.objects > 0 && self.fields.get(field).is_some_and(|f| f.seen == self.objects)
    }

    /// The enum values, when this place holds only a few distinct strings over many.
    fn enum_values(&self) -> Option<&[String]> {
        let only_strings = self.ints + self.nums + self.bools + self.arrays + self.objects == 0;
        let cap = (self.strs / 2).min(8);
        let plain = matches!(self.fmt, Fmt::Mixed | Fmt::Unset);
        if only_strings && self.strs >= 4 && !self.long && plain && !self.enums.is_empty() && self.enums.len() <= cap {
            Some(&self.enums)
        } else {
            None
        }
    }

    /// A JSON Schema (2020-12, as OpenAPI 3.1 uses it) of everything seen; `{}` when nothing was.
    pub fn schema(&self) -> Value {
        let mut m = Map::new();
        let mut types: Vec<&str> = Vec::new();
        if self.bools > 0 {
            types.push("boolean");
        }
        if self.nums > 0 {
            types.push("number");
        } else if self.ints > 0 {
            types.push("integer");
        }
        if self.strs > 0 {
            types.push("string");
        }
        if self.arrays > 0 {
            types.push("array");
        }
        if self.objects > 0 {
            types.push("object");
        }
        if self.nulls > 0 {
            types.push("null");
        }
        if types.is_empty() {
            return Value::Object(m);
        }
        let ty = if types.len() == 1 {
            Value::from(types[0])
        } else {
            Value::Array(types.iter().map(|t| Value::from(*t)).collect())
        };
        m.insert("type".into(), ty);
        if self.strs > 0 {
            if let Some(f) = self.fmt.schema_format() {
                m.insert("format".into(), Value::from(f));
            } else if self.fmt == Fmt::Digits {
                m.insert("pattern".into(), Value::from("^[0-9]+$"));
            }
        }
        if let Some(e) = self.enum_values() {
            let mut vals: Vec<Value> = e.iter().map(|s| Value::String(s.clone())).collect();
            if self.nulls > 0 {
                vals.push(Value::Null);
            }
            m.insert("enum".into(), Value::Array(vals));
        }
        if let Some(ex) = &self.example {
            m.insert("examples".into(), Value::Array(vec![ex.clone()]));
        }
        if self.arrays > 0 {
            let items = self.items.as_ref().map_or_else(|| Value::Object(Map::new()), |i| i.schema());
            m.insert("items".into(), items);
        }
        if self.objects > 0 {
            if !self.fields.is_empty() {
                let mut props = Map::new();
                let mut required = Vec::new();
                for (k, f) in &self.fields {
                    props.insert(k.clone(), f.schema());
                    if self.required(k) {
                        required.push(Value::String(k.clone()));
                    }
                }
                m.insert("properties".into(), Value::Object(props));
                if !required.is_empty() {
                    m.insert("required".into(), Value::Array(required));
                }
            }
            if let Some(d) = &self.dict {
                m.insert("additionalProperties".into(), d.schema());
            }
        }
        Value::Object(m)
    }

    /// One line that shows the shape, such as `{id: int, name: str, tags: [str], next?: str|null, kind: "a"|"b"}`.
    /// It starts four levels deep and goes shallower until it fits in `budget` characters, then cuts with `…`.
    pub fn sketch(&self, budget: usize) -> String {
        if budget == 0 {
            return String::new();
        }
        let mut out = String::new();
        for depth in (0..=SKETCH_DEPTH).rev() {
            out = self.render(depth);
            if out.chars().count() <= budget {
                return out;
            }
        }
        let mut cut: String = out.chars().take(budget - 1).collect();
        cut.push('…');
        cut
    }

    fn render(&self, depth: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.bools > 0 {
            parts.push("bool".into());
        }
        if self.nums > 0 {
            parts.push("num".into());
        } else if self.ints > 0 {
            parts.push("int".into());
        }
        if self.strs > 0 {
            if let Some(e) = self.enum_values() {
                let lits: Vec<String> = e.iter().map(|s| format!("\"{s}\"")).collect();
                parts.push(lits.join("|"));
            } else {
                parts.push(
                    match self.fmt {
                        Fmt::Uuid => "uuid",
                        Fmt::DateTime => "time",
                        Fmt::Date => "date",
                        Fmt::Email => "email",
                        Fmt::Uri => "url",
                        _ => "str",
                    }
                    .into(),
                );
            }
        }
        if self.arrays > 0 {
            parts.push(match &self.items {
                Some(i) => format!("[{}]", i.render(depth)),
                None => "[]".into(),
            });
        }
        if self.objects > 0 {
            parts.push(self.render_object(depth));
        }
        if self.nulls > 0 {
            parts.push("null".into());
        }
        if parts.is_empty() {
            "any".into()
        } else {
            parts.join("|")
        }
    }

    fn render_object(&self, depth: usize) -> String {
        if self.fields.is_empty() {
            return match &self.dict {
                Some(d) => format!("{{[id]: {}}}", d.render(depth.saturating_sub(1))),
                None => "{}".into(),
            };
        }
        if depth == 0 {
            return format!("{{…{}}}", self.fields.len());
        }
        let mut items: Vec<String> = self
            .fields
            .iter()
            .take(SKETCH_FIELDS)
            .map(|(k, f)| {
                let opt = if self.required(k) { "" } else { "?" };
                format!("{k}{opt}: {}", f.render(depth - 1))
            })
            .collect();
        if self.fields.len() > SKETCH_FIELDS {
            items.push(format!("…+{}", self.fields.len() - SKETCH_FIELDS));
        }
        format!("{{{}}}", items.join(", "))
    }

    /// Calls `f(path, key, shape)` for every object field down to four levels, such as `("$.data.next", "next", ..)`;
    /// the items of an array are entered as `[]` (`$.data[].id`).
    pub fn walk(&self, f: &mut dyn FnMut(&str, &str, &Shape)) {
        self.walk_at("$", 0, f);
    }

    fn walk_at(&self, path: &str, depth: usize, f: &mut dyn FnMut(&str, &str, &Shape)) {
        if depth < SKETCH_DEPTH {
            for (k, s) in &self.fields {
                let p = format!("{path}.{k}");
                f(&p, k, s);
                s.walk_at(&p, depth + 1, f);
            }
        }
        if let Some(i) = &self.items {
            i.walk_at(&format!("{path}[]"), depth, f);
        }
    }
}

/// An example, cut at 60 characters.
fn cut_example(s: &str) -> String {
    if s.chars().count() > EXAMPLE_CAP {
        let mut c: String = s.chars().take(EXAMPLE_CAP).collect();
        c.push('…');
        c
    } else {
        s.to_string()
    }
}

/// The format of one string; `Mixed` when it has none.
fn fmt_of(s: &str) -> Fmt {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|x| x.iter().all(u8::is_ascii_digit));
    let date_at_start = b.len() >= 10 && digits(0..4) && b[4] == b'-' && digits(5..7) && b[7] == b'-' && digits(8..10);
    if is_uuid(s) {
        Fmt::Uuid
    } else if date_at_start && b.len() == 10 {
        Fmt::Date
    } else if date_at_start && b.len() >= 19 && b[10] == b'T' && digits(11..13) && b[13] == b':' && digits(14..16) {
        Fmt::DateTime
    } else if (s.starts_with("http://") || s.starts_with("https://")) && s.len() > 8 && !s.contains(char::is_whitespace)
    {
        Fmt::Uri
    } else if is_email(s) {
        Fmt::Email
    } else if !s.is_empty() && b.iter().all(u8::is_ascii_digit) {
        Fmt::Digits
    } else {
        Fmt::Mixed
    }
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter()
            .enumerate()
            .all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { *c == b'-' } else { c.is_ascii_hexdigit() })
}

fn is_email(s: &str) -> bool {
    if s.contains(char::is_whitespace) || s.matches('@').count() != 1 {
        return false;
    }
    match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
        }
        None => false,
    }
}

/// Whether a key looks like an id: only digits, a UUID, or 12 or more hex digits.
fn id_like(k: &str) -> bool {
    (!k.is_empty() && k.bytes().all(|c| c.is_ascii_digit()))
        || is_uuid(k)
        || (k.len() >= 12 && k.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// Whether an object is a dictionary keyed by ids: 8 keys or more, 80 % of them ids.
fn is_dict(m: &Map<String, Value>) -> bool {
    m.len() >= DICT_MIN && m.keys().filter(|k| id_like(k)).count() * 5 >= m.len() * 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn merged(samples: &[Value]) -> Shape {
        let mut s = Shape::default();
        for v in samples {
            s.add(v);
        }
        s
    }

    #[test]
    fn optional_and_nullable_fields_are_told_apart() {
        let s = merged(&[
            json!({"id": 1, "name": "a", "next": "u"}),
            json!({"id": 2, "name": "b", "next": null}),
            json!({"id": 3, "name": "c"}),
        ]);
        assert!(s.required("id"));
        assert!(s.required("name"));
        assert!(!s.required("next"));
        assert!(!s.required("missing"));
        assert_eq!(s.fields["next"].nulls, 1);
        assert_eq!(s.fields["next"].seen, 2);
        let schema = s.schema();
        assert_eq!(schema["required"], json!(["id", "name"]));
        assert_eq!(schema["properties"]["next"]["type"], json!(["string", "null"]));
        assert_eq!(schema["properties"]["id"]["type"], json!("integer"));
    }

    #[test]
    fn numbers_with_a_fraction_make_a_number() {
        let s = merged(&[json!(1), json!(2.5)]);
        assert_eq!(s.ints, 1);
        assert_eq!(s.nums, 1);
        assert_eq!(s.schema()["type"], json!("number"));
        assert_eq!(s.sketch(40), "num");
        assert_eq!(Shape::of(&json!(true)).schema()["type"], json!("boolean"));
        assert_eq!(Shape::of(&json!(true)).sketch(40), "bool");
    }

    #[test]
    fn nothing_seen_is_an_empty_schema() {
        assert_eq!(Shape::default().schema(), json!({}));
        assert_eq!(Shape::default().sketch(40), "any");
        assert_eq!(Shape::of(&json!(null)).schema(), json!({"type": "null"}));
    }

    #[test]
    fn a_few_distinct_strings_make_an_enum() {
        let s = merged(&["a", "b", "a", "b", "a", "b"].map(|x| json!(x)));
        assert_eq!(s.schema()["enum"], json!(["a", "b"]));
        assert_eq!(s.sketch(40), "\"a\"|\"b\"");
        let with_null = merged(&[json!("a"), json!("b"), json!("a"), json!("b"), json!(null)]);
        assert_eq!(with_null.schema()["enum"], json!(["a", "b", null]));
        assert_eq!(with_null.schema()["type"], json!(["string", "null"]));
    }

    #[test]
    fn many_distinct_strings_or_too_few_samples_make_no_enum() {
        let distinct = merged(&["a", "b", "c", "d"].map(|x| json!(x)));
        assert!(distinct.schema().get("enum").is_none());
        let few = merged(&[json!("a"), json!("a"), json!("a")]);
        assert!(few.schema().get("enum").is_none());
        let nine: Vec<Value> = (0..30).map(|i| json!(format!("v{}", i % 9))).collect();
        let s = merged(&nine);
        assert_eq!(s.enums.len(), 9);
        assert!(s.schema().get("enum").is_none());
        let eight: Vec<Value> = (0..30).map(|i| json!(format!("v{}", i % 8))).collect();
        assert_eq!(merged(&eight).schema()["enum"].as_array().map(Vec::len), Some(8));
    }

    #[test]
    fn a_long_string_or_a_format_blocks_the_enum() {
        let long = "x".repeat(40);
        let s = merged(&[json!("a"), json!("a"), json!("a"), json!("a"), json!(long)]);
        assert!(s.long);
        assert!(s.schema().get("enum").is_none());
        let dates = merged(&["2024-01-02", "2024-01-02", "2024-01-02", "2024-01-02"].map(|x| json!(x)));
        assert!(dates.schema().get("enum").is_none());
        assert_eq!(dates.schema()["format"], json!("date"));
    }

    #[test]
    fn string_formats_are_recognised() {
        let cases = [
            ("123e4567-e89b-12d3-a456-426614174000", "uuid", "uuid"),
            ("2024-05-01T10:20:30Z", "date-time", "time"),
            ("2024-05-01", "date", "date"),
            ("me@example.com", "email", "email"),
            ("https://example.com/a?b=c", "uri", "url"),
        ];
        for (text, format, word) in cases {
            let s = Shape::of(&json!(text));
            assert_eq!(s.schema()["format"], json!(format), "{text}");
            assert_eq!(s.sketch(40), word, "{text}");
        }
        let digits = Shape::of(&json!("12345"));
        assert_eq!(digits.schema()["pattern"], json!("^[0-9]+$"));
        assert!(digits.schema().get("format").is_none());
        assert_eq!(digits.sketch(40), "str");
        let plain = Shape::of(&json!("hello"));
        assert!(plain.schema().get("format").is_none());
        assert!(plain.schema().get("pattern").is_none());
    }

    #[test]
    fn formats_merge_to_mixed_when_they_differ() {
        assert_eq!(Fmt::Unset.merge(Fmt::Date), Fmt::Date);
        assert_eq!(Fmt::Date.merge(Fmt::Unset), Fmt::Date);
        assert_eq!(Fmt::Date.merge(Fmt::Date), Fmt::Date);
        assert_eq!(Fmt::Date.merge(Fmt::Uuid), Fmt::Mixed);
        let s = merged(&[json!("2024-05-01"), json!("hello")]);
        assert_eq!(s.fmt, Fmt::Mixed);
        assert!(s.schema().get("format").is_none());
    }

    #[test]
    fn an_object_keyed_by_ids_is_a_dictionary() {
        let mut m = Map::new();
        for i in 0..10 {
            m.insert((1000 + i).to_string(), json!({"name": format!("n{i}"), "n": i}));
        }
        let s = Shape::of(&Value::Object(m));
        assert!(s.fields.is_empty());
        let dict = s.dict.as_ref().expect("a dictionary");
        assert_eq!(dict.seen, 10);
        assert_eq!(s.schema()["additionalProperties"]["type"], json!("object"));
        assert!(s.schema().get("properties").is_none());
        assert!(s.sketch(80).starts_with("{[id]: {name: str, n: int}}"));
    }

    #[test]
    fn eighty_percent_of_ids_is_enough_and_seven_keys_are_not() {
        let mut m = Map::new();
        for i in 0..8 {
            m.insert(format!("{}", 5000 + i), json!(1));
        }
        m.insert("meta".into(), json!(1));
        let ids8_of_9 = Shape::of(&Value::Object(m.clone()));
        assert!(ids8_of_9.dict.is_some());
        m.insert("more".into(), json!(1));
        m.insert("other".into(), json!(1));
        assert!(Shape::of(&Value::Object(m)).dict.is_none());
        let mut few = Map::new();
        for i in 0..7 {
            few.insert(format!("{}", 5000 + i), json!(1));
        }
        assert!(Shape::of(&Value::Object(few)).dict.is_none());
        let mut names = Map::new();
        for i in 0..10 {
            names.insert(format!("field{i}"), json!(1));
        }
        assert!(Shape::of(&Value::Object(names)).dict.is_none());
    }

    #[test]
    fn uuid_and_long_hex_keys_count_as_ids() {
        assert!(id_like("123e4567-e89b-12d3-a456-426614174000"));
        assert!(id_like("deadbeefcafe"));
        assert!(id_like("42"));
        assert!(!id_like("deadbeef"));
        assert!(!id_like("name"));
        assert!(!id_like(""));
    }

    #[test]
    fn caps_count_what_was_cut() {
        let big: Vec<Value> = (0..60).map(|i| json!(i)).collect();
        let s = Shape::of(&Value::Array(big));
        assert_eq!(s.max_items, 60);
        assert_eq!(s.items.as_ref().map(|i| i.seen), Some(50));
        assert_eq!(s.cut, 10);
        let mut m = Map::new();
        for i in 0..250 {
            m.insert(format!("f{i}"), json!(i));
        }
        let o = Shape::of(&Value::Object(m));
        assert_eq!(o.fields.len(), 200);
        assert_eq!(o.cut, 50);
        let mut deep = json!(1);
        for _ in 0..15 {
            deep = json!({"a": deep});
        }
        let d = Shape::of(&deep);
        // what lies below the depth cap is counted where the cap falls: the object `MAX_DEPTH` levels down
        let mut at = &d;
        for _ in 0..MAX_DEPTH {
            at = &at.fields["a"];
        }
        assert_eq!(at.cut, 1);
    }

    #[test]
    fn an_empty_array_only_seen_empty_has_no_items() {
        let s = merged(&[json!({"tags": []}), json!({"tags": []})]);
        assert_eq!(s.sketch(40), "{tags: []}");
        assert_eq!(s.schema()["properties"]["tags"]["items"], json!({}));
        assert_eq!(s.schema()["properties"]["tags"]["type"], json!("array"));
    }

    #[test]
    fn the_schema_of_nested_data_has_items_properties_and_examples() {
        let s = merged(&[json!({"data": [{"id": 1, "name": "ann"}]}), json!({"data": [{"id": 2, "name": "bob"}]})]);
        let schema = s.schema();
        assert_eq!(schema["type"], json!("object"));
        let item = &schema["properties"]["data"]["items"];
        assert_eq!(item["properties"]["id"]["type"], json!("integer"));
        assert_eq!(item["properties"]["id"]["examples"], json!([1]));
        assert_eq!(item["properties"]["name"]["examples"], json!(["ann"]));
        assert_eq!(item["required"], json!(["id", "name"]));
    }

    #[test]
    fn the_sketch_lists_types_optionals_and_nulls() {
        let s = merged(&[
            json!({"id": 1, "name": "n", "tags": ["a"], "next": "u"}),
            json!({"id": 2, "name": "m", "tags": [], "next": null}),
            json!({"id": 3, "name": "o", "tags": ["b"]}),
        ]);
        assert_eq!(s.sketch(100), "{id: int, name: str, tags: [str], next?: str|null}");
    }

    #[test]
    fn the_sketch_shows_enums_and_dates() {
        let s = merged(&[
            json!({"kind": "a", "at": "2024-05-01T10:20:30Z"}),
            json!({"kind": "b", "at": "2024-05-02T10:20:30Z"}),
            json!({"kind": "a", "at": "2024-05-03T10:20:30Z"}),
            json!({"kind": "b", "at": "2024-05-04T10:20:30Z"}),
        ]);
        assert_eq!(s.sketch(100), "{kind: \"a\"|\"b\", at: time}");
    }

    #[test]
    fn the_sketch_goes_shallower_until_it_fits() {
        let s = Shape::of(&json!({"a": {"b": {"c": {"d": {"e": 1}}}}}));
        assert_eq!(s.sketch(100), "{a: {b: {c: {d: {…1}}}}}");
        assert_eq!(s.sketch(19), "{a: {b: {c: {…1}}}}");
        assert_eq!(s.sketch(14), "{a: {b: {…1}}}");
        assert_eq!(s.sketch(12), "{a: {…1}}");
        assert_eq!(s.sketch(5), "{…1}");
    }

    #[test]
    fn the_sketch_is_cut_with_an_ellipsis_when_nothing_fits() {
        let s = Shape::of(&json!({"alpha": 1, "beta": 2, "gamma": 3}));
        let out = s.sketch(3);
        assert_eq!(out.chars().count(), 3);
        assert!(out.ends_with('…'));
        assert_eq!(s.sketch(0), "");
    }

    #[test]
    fn the_sketch_lists_twelve_fields_and_counts_the_rest() {
        let mut m = Map::new();
        for i in 0..14 {
            m.insert(format!("f{i}"), json!(i));
        }
        let out = Shape::of(&Value::Object(m)).sketch(1000);
        assert!(out.starts_with("{f0: int, f1: int"));
        assert!(out.contains("f11: int"));
        assert!(!out.contains("f12"));
        assert!(out.ends_with(", …+2}"));
    }

    #[test]
    fn walk_visits_fields_and_array_items_down_to_four_levels() {
        let s = Shape::of(&json!({"data": {"next": "x", "items": [{"id": 1}]}}));
        let mut seen = Vec::new();
        s.walk(&mut |path, key, _| seen.push((path.to_string(), key.to_string())));
        let paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["$.data", "$.data.next", "$.data.items", "$.data.items[].id"]);
        assert_eq!(seen[1].1, "next");
        let deep = Shape::of(&json!({"a": {"b": {"c": {"d": {"e": {"f": 1}}}}}}));
        let mut count = 0;
        deep.walk(&mut |_, _, _| count += 1);
        assert_eq!(count, 4);
        let root_array = Shape::of(&json!([{"id": 1}]));
        let mut ids = Vec::new();
        root_array.walk(&mut |p, _, _| ids.push(p.to_string()));
        assert_eq!(ids, ["$[].id"]);
    }

    #[test]
    fn an_example_is_cut_at_sixty_characters() {
        let text = "word ".repeat(20);
        let s = Shape::of(&json!(text));
        let ex = s.example.as_ref().and_then(Value::as_str).unwrap_or("").to_string();
        assert_eq!(ex.chars().count(), 61);
        assert!(ex.ends_with('…'));
    }

    const TOKEN: &str = "Zm9vYmFy_BazQux1234ABCDefgh-ij";

    #[test]
    fn an_opaque_string_never_appears_in_schema_or_sketch() {
        assert!(opaque(TOKEN));
        let samples: Vec<Value> = (0..6).map(|_| json!({"blob": TOKEN, "list": [TOKEN], "ok": "fine"})).collect();
        let s = merged(&samples);
        assert_eq!(s.fields["blob"].example, Some(json!("<opaque>")));
        assert!(s.fields["blob"].enums.is_empty());
        let schema = s.schema().to_string();
        assert!(!schema.contains(TOKEN));
        assert!(schema.contains("<opaque>"));
        assert!(!s.sketch(1000).contains(TOKEN));
        assert!(!format!("{s:?}").contains(TOKEN));
    }

    #[test]
    fn an_opaque_field_name_is_hidden_too() {
        let mut m = Map::new();
        m.insert(TOKEN.to_string(), json!(1));
        m.insert("id".to_string(), json!(2));
        let s = Shape::of(&Value::Object(m));
        assert!(!s.schema().to_string().contains(TOKEN));
        assert!(!s.sketch(1000).contains(TOKEN));
        assert!(s.fields.contains_key("<opaque>"));
    }

    #[test]
    fn a_real_example_replaces_an_opaque_placeholder() {
        let s = merged(&[json!(TOKEN), json!("hello")]);
        assert_eq!(s.example, Some(json!("hello")));
        let t = merged(&[json!("hello"), json!(TOKEN)]);
        assert_eq!(t.example, Some(json!("hello")));
    }

    #[test]
    fn a_secret_stays_as_it_is_and_is_no_enum_value() {
        let samples: Vec<Value> = (0..6).map(|_| json!({"token": SECRET})).collect();
        let s = merged(&samples);
        let f = &s.fields["token"];
        assert_eq!(f.example, Some(json!("<secret>")));
        assert!(f.enums.is_empty());
        let schema = s.schema();
        assert_eq!(schema["properties"]["token"]["examples"], json!(["<secret>"]));
        assert!(schema["properties"]["token"].get("enum").is_none());
        assert_eq!(s.sketch(100), "{token: str}");
    }
}
