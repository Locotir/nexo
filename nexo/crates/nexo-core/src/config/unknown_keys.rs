use super::*;

/// Copies onto `base` the value `over` has for each key `said` holds, table into table.
/// A key `over` lacks (unknown to nexo, or left out of an untrusted project) changes nothing.
pub(super) fn overlay(base: &mut toml::Table, over: &toml::Table, said: &toml::Table) {
    for (k, s) in said {
        let Some(o) = over.get(k) else { continue };
        match (s, o, base.get_mut(k)) {
            (toml::Value::Table(s), toml::Value::Table(o), Some(toml::Value::Table(b))) => overlay(b, o, s),
            _ => {
                base.insert(k.clone(), o.clone());
            }
        }
    }
}

/// The keys of a config file that nexo does not read (a typo, a section that does not exist, a
/// setting since retired), said in one line; None when it reads them all. Nothing stops for them:
/// the file still loads, but a setting that silently does nothing is worse than a warning.
pub fn unread_keys(text: &str, parsed: &Config) -> Option<String> {
    let raw: toml::Table = toml::from_str(text).ok()?;
    let toml::Value::Table(read) = toml::Value::try_from(parsed).ok()? else {
        return None;
    };
    let mut out = Vec::new();
    unread_in(&raw, &read, "", &mut out);
    if out.is_empty() {
        return None;
    }
    let (it, does) = if out.len() == 1 { ("it", "it does") } else { ("them", "they do") };
    Some(format!("{}: nexo does not read {it}, so {does} nothing", out.join(", ")))
}

pub(super) fn unread_in(raw: &toml::Table, read: &toml::Table, at: &str, out: &mut Vec<String>) {
    for (k, v) in raw {
        let path = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
        match (v, read.get(k)) {
            (toml::Value::Table(r), Some(toml::Value::Table(s))) => unread_in(r, s, &path, out),
            (toml::Value::Array(r), Some(toml::Value::Array(s))) => {
                for (i, (a, b)) in r.iter().zip(s).enumerate() {
                    if let (toml::Value::Table(a), toml::Value::Table(b)) = (a, b) {
                        unread_in(a, b, &format!("{path}[{i}]"), out);
                    }
                }
            }
            (_, Some(_)) => {}
            (_, None) => {
                let near = read.keys().filter(|s| near(k, s)).min_by_key(|s| distance(k, s));
                out.push(match near {
                    Some(s) => format!("{path} (did you mean {s}?)"),
                    None if k.contains("curator") || k == "spec" => {
                        format!("{path} (the curator's settings are [context] curator_spec)")
                    }
                    None => path,
                });
            }
        }
    }
}

/// Two keys close enough that one is likely a slip for the other.
pub(super) fn near(a: &str, b: &str) -> bool {
    distance(a, b) <= 2.max(a.len().min(b.len()) / 5)
}

/// Edits between two keys (insert, delete, replace one character).
pub(super) fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}
