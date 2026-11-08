//! Edits to a configuration file that keep everything else in it: comments,
//! order, formatting. Used to remember permission rules, prices and the
//! limits learned from a provider's errors.

use std::path::Path;

use nexo_core::pricing::Pricing;
use toml_edit::{value, Array, DocumentMut, Item, Table, Value};

fn load(file: &Path) -> anyhow::Result<DocumentMut> {
    // only a file that is not there starts empty: one that cannot be read (not UTF-8, locked,
    // no permission) is an error, or the save that follows would write over everything in it
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => anyhow::bail!("cannot read {}: {e}", file.display()),
    };
    Ok(text.parse::<DocumentMut>()?)
}

fn save(file: &Path, doc: &DocumentMut) -> anyhow::Result<()> {
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    // a linked file (dotfiles kept elsewhere) is written where it lives, the link stays, and so do
    // its permissions
    let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let perms = std::fs::metadata(&file).ok().map(|m| m.permissions());
    let tmp = file.with_extension(format!("toml.{}.tmp", std::process::id()));
    std::fs::write(&tmp, doc.to_string())?;
    if let Some(p) = perms {
        std::fs::set_permissions(&tmp, p)?;
    }
    std::fs::rename(&tmp, &file)?;
    Ok(())
}

/// The table at `path`, created on the way (intermediate tables stay
/// implicit, so no empty `[providers]` header appears).
fn table_at<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> &'a mut Table {
    let mut t: &mut Table = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        let item = t.entry(seg).or_insert_with(|| Item::Table(Table::new()));
        if !item.is_table() {
            *item = Item::Table(Table::new());
        }
        let tt = item.as_table_mut().expect("a table");
        if i + 1 < path.len() && tt.is_empty() {
            tt.set_implicit(true);
        }
        t = tt;
    }
    t
}

/// Sets `key = v` in the table at `path`.
pub fn set(file: &Path, path: &[&str], key: &str, v: Value) -> anyhow::Result<()> {
    let mut doc = load(file)?;
    table_at(&mut doc, path)[key] = value(v);
    save(file, &doc)
}

/// Appends `s` to the array `key` of the table at `path`, unless it is there.
pub fn push(file: &Path, path: &[&str], key: &str, s: &str) -> anyhow::Result<()> {
    let mut doc = load(file)?;
    let t = table_at(&mut doc, path);
    let item = t.entry(key).or_insert_with(|| value(Array::new()));
    if !item.is_array() {
        *item = value(Array::new());
    }
    let arr = item.as_array_mut().expect("an array");
    if !arr.iter().any(|v| v.as_str() == Some(s)) {
        arr.push(s);
    }
    save(file, &doc)
}

/// `[providers.<provider>.pricing."<model>"]` with the four prices.
pub fn set_pricing(file: &Path, provider: &str, model: &str, p: Pricing) -> anyhow::Result<()> {
    let mut doc = load(file)?;
    let t = table_at(&mut doc, &["providers", provider, "pricing", model]);
    t["input"] = value(p.input);
    t["output"] = value(p.output);
    t["cache_read"] = value(p.cache_read);
    t["cache_write"] = value(p.cache_write);
    save(file, &doc)
}

/// `[providers.<provider>.<which>]` `"<model>" = n` (`which`: `context_window` or `max_output`).
pub fn set_limit(file: &Path, provider: &str, model: &str, which: &str, n: u32) -> anyhow::Result<()> {
    let mut doc = load(file)?;
    table_at(&mut doc, &["providers", provider, which])[model] = value(i64::from(n));
    save(file, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_comments_and_adds_nested_tables() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        std::fs::write(&f, "# mine\nprovider = \"deepseek\" # chosen\n\n[ui]\ntheme = \"light\"\n").unwrap();
        set_pricing(
            &f,
            "deepseek",
            "deepseek-chat",
            Pricing { input: 0.28, output: 0.42, cache_read: 0.028, cache_write: 0.0 },
        )
        .unwrap();
        set_limit(&f, "deepseek", "deepseek-chat", "context_window", 131_072).unwrap();
        set_limit(&f, "deepseek", "deepseek-chat", "max_output", 8_192).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.starts_with("# mine\nprovider = \"deepseek\" # chosen\n"), "{t}");
        // a plain model name is a bare key; one with dots or slashes gets quoted
        assert!(t.contains("[providers.deepseek.pricing.deepseek-chat]"), "{t}");
        assert!(!t.contains("\n[providers]\n"), "no empty header: {t}");
        set_limit(&f, "fireworks", "accounts/fireworks/models/kimi-k2", "context_window", 131_072).unwrap();
        set_pricing(&f, "openai", "gpt-5.1", Pricing { input: 1.25, ..Default::default() }).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.contains("[providers.openai.pricing.\"gpt-5.1\"]"), "{t}");
        assert!(t.contains("\"accounts/fireworks/models/kimi-k2\" = 131072"), "{t}");
        let cfg: nexo_core::Config = nexo_core::Config::parse(&t).unwrap();
        let p = &cfg.providers["deepseek"];
        assert_eq!(p.pricing_for("deepseek-chat").unwrap().output, 0.42);
        assert_eq!(p.context_window["deepseek-chat"], 131_072);
        assert_eq!(p.max_output["deepseek-chat"], 8_192);
        assert_eq!(cfg.providers["openai"].pricing_for("gpt-5.1").unwrap().input, 1.25);
        assert_eq!(cfg.providers["fireworks"].context_window["accounts/fireworks/models/kimi-k2"], 131_072);
        assert_eq!(cfg.ui.theme, "light");
        // a second write replaces the value, keeping one table
        set_limit(&f, "deepseek", "deepseek-chat", "context_window", 65_536).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(t.matches("[providers.deepseek.context_window]").count(), 1, "{t}");
        assert!(t.contains("65536"));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("config.toml");
        // saved by an editor in another encoding: not UTF-8, so not readable as text
        let bytes: Vec<u8> = b"provider = \"deepseek\" # caf\xe9\n".to_vec();
        std::fs::write(&f, &bytes).unwrap();
        assert!(push(&f, &["permissions"], "allow", "read").is_err());
        assert!(set(&f, &["agent"], "max_tokens", Value::from(4096)).is_err());
        assert_eq!(std::fs::read(&f).unwrap(), bytes, "nothing was written over it");
        // a file that is not there yet is made
        let new = d.path().join("new.toml");
        push(&new, &["permissions"], "allow", "read").unwrap();
        assert!(std::fs::read_to_string(&new).unwrap().contains("read"));
    }

    #[test]
    fn pushes_once_and_creates_the_file() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("sub").join("config.toml");
        push(&f, &["permissions"], "allow", "bash(git *)").unwrap();
        push(&f, &["permissions"], "allow", "bash(git *)").unwrap();
        push(&f, &["permissions"], "allow", "read").unwrap();
        let cfg = nexo_core::Config::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["bash(git *)", "read"]);
        set(&f, &["agent"], "max_tokens", Value::from(4096)).unwrap();
        let cfg = nexo_core::Config::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(cfg.agent.max_tokens, 4096);
    }
    #[cfg(unix)]
    #[test]
    fn a_linked_file_is_written_through_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("dotfiles-config.toml");
        std::fs::write(&real, "# mine\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        set(&link, &["agent"], "max_tokens", Value::from(4096)).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays");
        assert!(std::fs::read_to_string(&real).unwrap().contains("max_tokens = 4096"));
        assert_eq!(std::fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
