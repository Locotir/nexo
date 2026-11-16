//! The audit log: one JSON line per decision of the supervisor, in `audit.jsonl`, rotated to `audit.jsonl.1`
//! at 5 MiB. The text kept is short and redacted; the `outcome` of a line is filled in later, when it is
//! known what the decision led to.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{input_hash, Secs};

pub const ROTATE_AT: u64 = 5 * 1024 * 1024;
const TEXT_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: Secs,
    pub agent_key: String,
    pub source: String,
    pub state: String,
    pub action: String,
    pub rule: String,
    pub text_hash: String,
    pub text: String,
    pub cost_usd: f64,
    pub outcome: Option<String>,
}

impl Entry {
    /// A line for a decision; the text is redacted and cut here.
    pub fn new(ts: Secs, key: &str, source: &str, state: &str, action: &str, rule: &str, text: &str) -> Entry {
        let clean = nexo_core::redact::redact(text);
        let short: String = clean.chars().take(TEXT_CHARS).collect();
        Entry {
            ts,
            agent_key: key.to_string(),
            source: source.to_string(),
            state: state.to_string(),
            action: action.to_string(),
            rule: rule.to_string(),
            text_hash: format!("{:016x}", input_hash(text)),
            text: short,
            cost_usd: 0.0,
            outcome: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Audit {
    path: PathBuf,
    rotate_at: u64,
}

impl Audit {
    pub fn new(path: impl Into<PathBuf>) -> Audit {
        Audit { path: path.into(), rotate_at: ROTATE_AT }
    }

    pub fn with_limit(mut self, bytes: u64) -> Audit {
        self.rotate_at = bytes;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn older(&self) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(".1");
        PathBuf::from(s)
    }

    pub fn append(&self, e: &Entry) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        self.rotate()?;
        let mut line = serde_json::to_vec(e)?;
        line.push(b'\n');
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(&line)
    }

    /// Past the limit, the log becomes `.1` (the one before is dropped).
    pub fn rotate(&self) -> std::io::Result<()> {
        match std::fs::metadata(&self.path) {
            Ok(m) if m.len() >= self.rotate_at => std::fs::rename(&self.path, self.older()),
            _ => Ok(()),
        }
    }

    /// Every line, oldest first (the older file first). Lines that do not parse are skipped.
    pub fn read(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for p in [self.older(), self.path.clone()] {
            if let Ok(text) = std::fs::read_to_string(p) {
                out.extend(text.lines().filter_map(|l| serde_json::from_str::<Entry>(l).ok()));
            }
        }
        out
    }

    /// Fill in the outcome of the latest line of `key` that has none. False when there was none.
    pub fn close_outcome(&self, key: &str, outcome: &str) -> std::io::Result<bool> {
        let text = std::fs::read_to_string(&self.path)?;
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        let mut done = false;
        for l in lines.iter_mut().rev() {
            let Ok(mut e) = serde_json::from_str::<Entry>(l) else { continue };
            if e.agent_key == key && e.outcome.is_none() {
                e.outcome = Some(outcome.to_string());
                *l = serde_json::to_string(&e)?;
                done = true;
                break;
            }
        }
        if done {
            let tmp = self.path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
            std::fs::write(&tmp, lines.join("\n") + "\n")?;
            std::fs::rename(&tmp, &self.path)?;
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(key: &str, text: &str) -> Entry {
        Entry::new(10, key, "rules", "stalled", "nudge", "stalled", text)
    }

    #[test]
    fn lines_are_redacted_cut_and_hashed() {
        let secret = format!("sk-{}", "a1B2c3D4".repeat(5));
        let long = format!("{secret} {}", "x".repeat(500));
        let x = e("k", &long);
        assert!(!x.text.contains(&secret));
        assert!(x.text.chars().count() <= TEXT_CHARS);
        assert_eq!(x.text_hash.len(), 16);
    }

    #[test]
    fn append_then_close_the_latest_open_line_of_that_session_only() {
        let d = tempfile::tempdir().unwrap();
        let a = Audit::new(d.path().join("sup").join("audit.jsonl"));
        a.append(&e("k1", "one")).unwrap();
        a.append(&e("k2", "two")).unwrap();
        a.append(&e("k1", "three")).unwrap();
        assert!(a.close_outcome("k1", "resumed").unwrap());
        let all = a.read();
        assert_eq!(all[0].outcome, None);
        assert_eq!(all[1].outcome, None);
        assert_eq!(all[2].outcome.as_deref(), Some("resumed"));
        assert!(a.close_outcome("k1", "no_effect").unwrap());
        assert_eq!(a.read()[0].outcome.as_deref(), Some("no_effect"));
        assert!(!a.close_outcome("k1", "again").unwrap());
        assert!(!a.close_outcome("nobody", "x").unwrap());
    }

    #[test]
    fn the_log_rotates_at_the_limit_and_keeps_one_older_file() {
        let d = tempfile::tempdir().unwrap();
        let a = Audit::new(d.path().join("audit.jsonl")).with_limit(600);
        for i in 0..12 {
            a.append(&e(&format!("k{i}"), "some text")).unwrap();
        }
        assert!(d.path().join("audit.jsonl.1").exists());
        let live = std::fs::metadata(a.path()).unwrap().len();
        assert!(live < 1200, "{live}");
        assert!(a.read().len() >= 6);
    }

    #[test]
    fn the_default_limit_is_five_mebibytes() {
        assert_eq!(ROTATE_AT, 5 * 1024 * 1024);
    }
}
