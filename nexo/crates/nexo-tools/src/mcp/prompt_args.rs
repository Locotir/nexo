use std::collections::BTreeMap;

use super::types::PromptDef;

/// The words of `input`, a quoted phrase being one.
fn words(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut cur, mut quote, mut any) = (String::new(), None::<char>, false);
    for c in input.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The arguments of a prompt from what was typed after its command: `key=value` for a name the
/// prompt has, the other words filling the remaining arguments in order. A missing required one is an error.
pub fn parse_prompt_args(def: &PromptDef, input: &str) -> Result<BTreeMap<String, String>, String> {
    let mut named = BTreeMap::new();
    let mut loose = Vec::new();
    for w in words(input) {
        match w.split_once('=') {
            Some((k, v)) if def.arguments.iter().any(|a| a.name == k) => {
                named.insert(k.to_string(), v.to_string());
            }
            _ => loose.push(w),
        }
    }
    let mut loose = loose.into_iter();
    for a in &def.arguments {
        if !named.contains_key(&a.name) {
            if let Some(w) = loose.next() {
                named.insert(a.name.clone(), w);
            }
        }
    }
    let rest: Vec<String> = loose.collect();
    if !rest.is_empty() {
        // a prompt with one free-text argument takes the rest of the line as it is
        if let Some(last) = def.arguments.last().and_then(|a| named.get_mut(&a.name)) {
            for w in rest {
                last.push(' ');
                last.push_str(&w);
            }
        } else {
            return Err(format!("`{}` takes no arguments", def.name));
        }
    }
    for a in &def.arguments {
        if a.required && !named.contains_key(&a.name) {
            return Err(format!("missing the argument `{}` of the prompt `{}`", a.name, def.name));
        }
    }
    Ok(named)
}
