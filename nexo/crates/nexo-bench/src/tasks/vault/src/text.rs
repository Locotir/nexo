//! Small text helpers shared by the layers: names, tags and display escaping.

/// The longest name `sanitize_name` returns.
pub const MAX_NAME: usize = 48;

/// The most tags one document keeps.
pub const MAX_TAGS: usize = 8;

/// Lower-cases `raw` and keeps only ASCII letters and digits; every other run of characters becomes
/// one `-`. The result never starts or ends with `-`, has at most `MAX_NAME` characters and is
/// never empty (`untitled` stands in).
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out.truncate(MAX_NAME);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Whether `name` can be a login: 3 to 24 letters, digits or underscores.
pub fn is_valid_username(name: &str) -> bool {
    (3..=24).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// At most `max` characters of `s`; a cut ends in `...`.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{keep}...")
}

/// `s` made safe to put in an HTML page.
pub fn escape_display(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The tags in a comma-separated list: sanitized, without duplicates, at most `MAX_TAGS`.
pub fn parse_tags(raw: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let tag = sanitize_name(part);
        if !tags.contains(&tag) && tags.len() < MAX_TAGS {
            tags.push(tag);
        }
    }
    tags
}

/// The `|`-separated fields of `s`, trimmed; at most `n`, the last one keeps any further `|`.
/// Missing fields come back empty, so the result always has exactly `n` entries.
pub fn split_fields(s: &str, n: usize) -> Vec<&str> {
    let mut fields: Vec<&str> = s.splitn(n, '|').map(str::trim).collect();
    fields.resize(n, "");
    fields
}

/// Whether `s` holds a control character (a line break, say).
pub fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}
