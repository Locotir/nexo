use super::*;

/// The first line of a request, at most 60 characters.
/// The sessions as a forest, parents before children (newest sibling first), each with its depth.
/// A session whose parent is gone is a root.
pub fn tree(metas: &[SessionMeta]) -> Vec<(usize, SessionMeta)> {
    let ids: std::collections::BTreeSet<&str> = metas.iter().map(|m| m.id.as_str()).collect();
    let mut sorted: Vec<&SessionMeta> = metas.iter().collect();
    sorted.sort_by(|a, b| b.updated.cmp(&a.updated));
    fn walk(out: &mut Vec<(usize, SessionMeta)>, all: &[&SessionMeta], m: &SessionMeta, depth: usize) {
        if out.iter().any(|(_, o)| o.id == m.id) {
            return;
        }
        out.push((depth, m.clone()));
        for c in all.iter().filter(|c| c.parent.as_deref() == Some(m.id.as_str())) {
            walk(out, all, c, depth + 1);
        }
    }
    let mut out = Vec::new();
    for m in &sorted {
        if !m.parent.as_deref().is_some_and(|p| ids.contains(p)) {
            walk(&mut out, &sorted, m, 0);
        }
    }
    out
}

pub fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = short_paths(line);
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    t
}

/// A long path written in full (`/home/me/shots/a.png`, quoted, `@`-prefixed or a `file://` URL), as a
/// dropped or attached file comes, shortened to its file name: the title keeps the words around it.
fn short_paths(line: &str) -> String {
    const LONG: usize = 30;
    let words: Vec<&str> = line.split_whitespace().collect();
    if !words.iter().any(|w| w.len() > LONG || w.starts_with(['\'', '"']) || w.starts_with("@'")) {
        return line.to_string();
    }
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let (at, rest) = w.strip_prefix('@').map_or(("", w), |r| ("@", r));
        let mut whole = rest.to_string();
        let mut j = i;
        if let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            while !(whole.len() > 1 && whole.ends_with(q)) && j + 1 < words.len() {
                j += 1;
                whole.push(' ');
                whole.push_str(words[j]);
            }
            if !(whole.len() > 1 && whole.ends_with(q)) {
                j = i;
                whole = rest.to_string();
            }
        }
        let inner = whole.trim_matches(['\'', '"']);
        let inner = inner.strip_prefix("file://").unwrap_or(inner);
        let name = inner.rsplit(['/', '\\']).find(|s| !s.is_empty());
        match name {
            Some(name) if inner.len() > LONG && absolute(inner) => {
                out.push(format!("{at}{name}"));
                i = j + 1;
            }
            _ => {
                out.push(w.to_string());
                i += 1;
            }
        }
    }
    out.join(" ")
}

/// `/a/b`, `~/a` or `C:\a`.
fn absolute(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with("~/")
        || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}
