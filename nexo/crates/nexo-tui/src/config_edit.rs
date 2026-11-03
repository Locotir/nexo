//! Small edits to the person's configuration file that keep the rest of it
//! as it is: one key under one table.

use std::path::Path;

/// Sets `key = "value"` under `[table]` in the TOML file at `path`, creating
/// the file or the table when missing and replacing the key when present.
/// Comments and other lines are kept.
pub fn set_string(path: &Path, table: &str, key: &str, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let out = with_key(&text, table, key, &format!("{key} = {}", toml_string(value)));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)
}

/// Sets `key = value` with `value` written as is (a bool, a number).
pub fn set_raw(path: &Path, table: &str, key: &str, value: &str) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let out = with_key(&text, table, key, &format!("{key} = {value}"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out)
}

fn toml_string(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `text` with `line` as the value of `key` in `[table]`.
pub fn with_key(text: &str, table: &str, key: &str, line: &str) -> String {
    let header = format!("[{table}]");
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let start = lines.iter().position(|l| l.trim() == header);
    match start {
        None => {
            if !text.is_empty() && !text.ends_with('\n') {
                lines.push(String::new());
            }
            if !lines.is_empty() && !lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(header);
            lines.push(line.to_string());
        }
        Some(s) => {
            let end = lines[s + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('['))
                .map(|i| s + 1 + i)
                .unwrap_or(lines.len());
            let existing = lines[s + 1..end].iter().position(|l| {
                let t = l.trim_start();
                t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
            });
            match existing {
                Some(i) => lines[s + 1 + i] = line.to_string(),
                None => {
                    // after the header and before the blank line that ends the table's block
                    let mut at = end;
                    while at > s + 1 && lines[at - 1].trim().is_empty() {
                        at -= 1;
                    }
                    lines.insert(at, line.to_string());
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_adds_and_creates_the_table() {
        let t = "provider = \"x\"\n\n[ui]\n# colours\ntheme = \"dark\"\nmouse = true\n\n[agent]\nmax_turns = 3\n";
        let out = with_key(t, "ui", "theme", "theme = \"nord\"");
        assert!(out.contains("# colours\ntheme = \"nord\"\nmouse = true\n\n[agent]"), "{out}");
        let out = with_key("[ui]\nmouse = true\n", "ui", "theme", "theme = \"nord\"");
        assert_eq!(out, "[ui]\nmouse = true\ntheme = \"nord\"\n");
        let out = with_key("provider = \"x\"\n", "ui", "theme", "theme = \"nord\"");
        assert_eq!(out, "provider = \"x\"\n\n[ui]\ntheme = \"nord\"\n");
        assert_eq!(with_key("", "ui", "theme", "theme = \"nord\""), "[ui]\ntheme = \"nord\"\n");
    }

    #[test]
    fn writes_the_file_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("cfg").join("config.toml");
        set_string(&p, "ui", "theme", "gruvbox").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[ui]\ntheme = \"gruvbox\"\n");
        std::fs::write(&p, "model = \"m\"\n[ui]\ntheme = \"gruvbox\"\n").unwrap();
        set_string(&p, "ui", "theme", "say \"hi\"").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "model = \"m\"\n[ui]\ntheme = \"say \\\"hi\\\"\"\n");
    }
}
