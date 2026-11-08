//! The mail adapter: reads the ledgers (`sent.jsonl`) and the receipts that nexo sessions write under the
//! mail folder and tells the supervisor which session wrote to which, and how far each mail got. It sees mail
//! between sessions in other terminals too. Read-only: it never writes into a mailbox.

use std::collections::HashMap;
use std::path::PathBuf;

use nexo_core::mail::Stage;
use nexo_core::mailbox::{read_ledgers, read_receipts};

use super::source::{one_line, Source, SourceKind};
use super::{ObsKind, Observation, Secs};

/// Mails older than this are not reported when the source starts (a session that is long gone leaves no trace).
const BACKFILL: Secs = 3600;
/// Characters of a mail's summary kept for the card.
const MAIL_SUMMARY: usize = 80;
/// Mails followed for their receipts at most.
const OPEN_MAX: usize = 200;

struct Open {
    from: String,
    to: String,
    stage: Stage,
}

pub struct MailSource {
    root: PathBuf,
    offsets: HashMap<String, u64>,
    open: HashMap<String, Open>,
    started: bool,
}

impl MailSource {
    /// Watch the mailboxes under `root` (the folder that holds one folder per session).
    pub fn new(root: impl Into<PathBuf>) -> MailSource {
        MailSource { root: root.into(), offsets: HashMap::new(), open: HashMap::new(), started: false }
    }
}

impl Source for MailSource {
    fn id(&self) -> &str {
        "mail-ledgers"
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Nexo
    }

    fn poll(&mut self, now: Secs) -> Vec<Observation> {
        let mut out = Vec::new();
        let first = !self.started;
        self.started = true;
        for r in read_ledgers(&self.root, &mut self.offsets) {
            let at = r.at / 1000;
            if first && now - at > BACKFILL {
                continue;
            }
            let mail = Some(r.id.clone());
            out.push(Observation {
                agent_key: r.from.clone(),
                at,
                kind: ObsKind::MsgOut {
                    to: r.to.clone(),
                    mail: mail.clone(),
                    summary: one_line(&r.summary, MAIL_SUMMARY),
                    bytes: r.bytes,
                },
            });
            out.push(Observation {
                agent_key: r.to.clone(),
                at,
                kind: ObsKind::MsgIn {
                    from: r.from.clone(),
                    mail,
                    summary: one_line(&r.summary, MAIL_SUMMARY),
                    held: false,
                },
            });
            if self.open.len() >= OPEN_MAX {
                if let Some(k) = self.open.keys().next().cloned() {
                    self.open.remove(&k);
                }
            }
            self.open.insert(r.id.clone(), Open { from: r.from, to: r.to, stage: Stage::Sent });
        }
        // receipts wait in the sender's folder
        let mut senders: Vec<String> = self.open.values().map(|o| o.from.clone()).collect();
        senders.sort();
        senders.dedup();
        for sid in senders {
            for (id, stage) in read_receipts(&self.root, &sid) {
                let Some(o) = self.open.get_mut(&id) else { continue };
                if !o.stage.advances_to(stage) {
                    continue;
                }
                o.stage = stage;
                out.push(Observation {
                    agent_key: o.to.clone(),
                    at: now,
                    kind: ObsKind::MsgStage { mail: id.clone(), stage },
                });
                if stage.is_final() {
                    self.open.remove(&id);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::mail::{FromKind, Inbound};
    use nexo_core::mailbox::{now_ms, Mailbox};

    fn kinds(o: &[Observation]) -> Vec<String> {
        o.iter()
            .map(|o| match &o.kind {
                ObsKind::MsgOut { .. } => format!("out@{}", o.agent_key),
                ObsKind::MsgIn { .. } => format!("in@{}", o.agent_key),
                ObsKind::MsgStage { stage, .. } => format!("{}@{}", stage.label(), o.agent_key),
                _ => "other".to_string(),
            })
            .collect()
    }

    #[test]
    fn it_sees_a_mail_and_its_receipts_between_two_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let a = Mailbox::open(dir.path(), "aaaa1111", "api", "/w/api", false, Inbound::Auto).unwrap();
        let b = Mailbox::open(dir.path(), "bbbb2222", "web", "/w/web", false, Inbound::Auto).unwrap();
        let mail = a.send("web", "please look at the build", None, FromKind::Agent).unwrap();
        let mut src = MailSource::new(dir.path());
        let first = src.poll(now_ms() / 1000);
        assert_eq!(kinds(&first), vec!["out@aaaa1111", "in@bbbb2222"], "{first:?}");
        assert!(src.poll(now_ms() / 1000).is_empty(), "nothing new");
        b.receipt(&mail, Stage::Delivered);
        let k = kinds(&src.poll(now_ms() / 1000));
        assert_eq!(k, vec!["delivered@bbbb2222"]);
        b.receipt(&mail, Stage::Read);
        assert_eq!(kinds(&src.poll(now_ms() / 1000)), vec!["read@bbbb2222"]);
        assert!(src.poll(now_ms() / 1000).is_empty(), "a final mail is no longer followed");
    }

    #[test]
    fn old_mail_is_not_replayed_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let a = Mailbox::open(dir.path(), "aaaa1111", "api", "/w/api", false, Inbound::Auto).unwrap();
        let _b = Mailbox::open(dir.path(), "bbbb2222", "web", "/w/web", false, Inbound::Auto).unwrap();
        a.send("web", "old news", None, FromKind::Agent).unwrap();
        let mut src = MailSource::new(dir.path());
        assert!(src.poll(now_ms() / 1000 + 2 * 3600).is_empty());
    }
}
