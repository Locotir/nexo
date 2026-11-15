//! The sent ledger (`sent.jsonl`) and the readers other sessions use over it and the receipts.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::*;

/// One line of `sent.jsonl`: what a session wrote to another (the text is not kept, only its summary).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SentRec {
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub from_name: String,
    pub to: String,
    #[serde(default)]
    pub to_name: String,
    pub summary: String,
    pub bytes: usize,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

impl Mailbox {
    pub(super) fn append_ledger(&self, rec: &SentRec) {
        let Ok(mut line) = serde_json::to_string(rec) else { return };
        line.push('\n');
        let mut o = fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        if let Ok(mut f) = o.open(self.dir().join("sent.jsonl")) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// The ledgers: every `sent.jsonl` under `root` read from `offsets` on (bytes, per session id), with the new offsets.
pub fn read_ledgers(root: &Path, offsets: &mut HashMap<String, u64>) -> Vec<SentRec> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let sid = e.file_name().to_string_lossy().to_string();
        let Ok(data) = fs::read(e.path().join("sent.jsonl")) else { continue };
        let off = offsets.entry(sid).or_insert(0);
        if (*off as usize) > data.len() {
            *off = 0;
        }
        let tail = &data[*off as usize..];
        // only whole lines
        let end = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        for line in tail[..end].split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            if let Ok(r) = serde_json::from_slice::<SentRec>(line) {
                out.push(r);
            }
        }
        *off += end as u64;
    }
    out.sort_by_key(|r| r.at);
    out
}

/// The receipts that wait in session `sid`'s folder: `(mail id, stage)`, the furthest stage of each.
pub fn read_receipts(root: &Path, sid: &str) -> Vec<(String, Stage)> {
    let mut best: HashMap<String, Stage> = HashMap::new();
    if let Ok(rd) = fs::read_dir(root.join(sid).join("receipts")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let Some((id, st)) = n.rsplit_once('.') else { continue };
            let Some(stage) = Stage::parse(st) else { continue };
            let slot = best.entry(id.to_string()).or_insert(stage);
            if stage.rank() > slot.rank() {
                *slot = stage;
            }
        }
    }
    let mut v: Vec<(String, Stage)> = best.into_iter().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// The ids of the session folders under `root`.
pub fn session_ids(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}
