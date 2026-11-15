use super::*;

pub(super) fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub(super) fn list_of(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            a.iter().map(|x| scalar(Some(x)).ok_or("array items must be strings or numbers".to_string())).collect()
        }
        Some(other) => scalar(Some(other)).map(|s| vec![s]).ok_or("expected a list of strings".to_string()),
    }
}

/// Split a command line into words; single and double quotes group words, a backslash escapes in double quotes.
pub(crate) fn split_words(s: &str) -> Result<Vec<String>, String> {
    let (mut words, mut cur, mut quote, mut has) = (Vec::new(), String::new(), None::<char>, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(n @ ('"' | '\\')) => cur.push(n),
                Some(n) => {
                    cur.push('\\');
                    cur.push(n);
                }
                None => cur.push('\\'),
            },
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("the command has an unclosed quote".to_string());
    }
    if has || !cur.is_empty() {
        words.push(cur);
    }
    Ok(words)
}
