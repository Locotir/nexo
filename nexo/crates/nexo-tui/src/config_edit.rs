//! Small edits to the person's configuration file that keep the rest of it
//! as it is: one key under one table. The editing itself lives in
//! `nexo_agent::config_edit`, so the terminal interface and the agent write
//! the file the same way.

use std::path::Path;

fn io(e: anyhow::Error) -> std::io::Error {
    std::io::Error::other(e)
}

/// Sets `key = "value"` under `[table]` in the TOML file at `path`, creating
/// the file or the table when missing and replacing the key when present.
/// Comments and other lines are kept.
pub fn set_string(path: &Path, table: &str, key: &str, value: &str) -> std::io::Result<()> {
    nexo_agent::config_edit::set_string(path, &[table], key, value).map_err(io)
}

/// Sets `key = value` with `value` written as is (a bool, a number).
pub fn set_raw(path: &Path, table: &str, key: &str, value: &str) -> std::io::Result<()> {
    nexo_agent::config_edit::set_raw(path, &[table], key, value).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` as the file after setting `theme = "nord"` under `[ui]`.
    fn edited(text: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        if !text.is_empty() {
            std::fs::write(&p, text).unwrap();
        }
        set_string(&p, "ui", "theme", "nord").unwrap();
        std::fs::read_to_string(&p).unwrap()
    }

    #[test]
    fn replaces_adds_and_creates_the_table() {
        let t = "provider = \"x\"\n\n[ui]\n# colours\ntheme = \"dark\"\nmouse = true\n\n[agent]\nmax_turns = 3\n";
        let out = edited(t);
        assert!(out.contains("# colours\ntheme = \"nord\"\nmouse = true\n\n[agent]"), "{out}");
        assert_eq!(edited("[ui]\nmouse = true\n"), "[ui]\nmouse = true\ntheme = \"nord\"\n");
        assert_eq!(edited("provider = \"x\"\n"), "provider = \"x\"\n\n[ui]\ntheme = \"nord\"\n");
        assert_eq!(edited(""), "[ui]\ntheme = \"nord\"\n");
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
        set_raw(&p, "ui", "mouse", "true").unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "model = \"m\"\n[ui]\ntheme = \"say \\\"hi\\\"\"\nmouse = true\n"
        );
    }
}
