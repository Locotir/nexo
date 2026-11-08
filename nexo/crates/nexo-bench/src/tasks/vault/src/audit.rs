//! A bounded log of what the users did, newest last.

use std::collections::VecDeque;

use crate::users::UserId;

/// One logged action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: u64,
    pub user: UserId,
    pub action: String,
    pub target: String,
}

/// The last `cap` entries.
#[derive(Debug)]
pub struct AuditLog {
    entries: VecDeque<Entry>,
    cap: usize,
    seq: u64,
}

impl AuditLog {
    /// A log that keeps at most `cap` entries.
    pub fn new(cap: usize) -> AuditLog {
        AuditLog { entries: VecDeque::new(), cap: cap.max(1), seq: 0 }
    }

    /// Adds an entry; control characters are replaced so one entry is always one line.
    pub fn record(&mut self, user: UserId, action: &str, target: &str) {
        let clean = |s: &str| s.chars().map(|c| if c.is_control() { '?' } else { c }).collect::<String>();
        self.seq += 1;
        if self.entries.len() == self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry { seq: self.seq, user, action: clean(action), target: clean(target) });
    }

    /// How many entries are kept.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is kept.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The newest entry.
    pub fn last(&self) -> Option<&Entry> {
        self.entries.back()
    }

    /// The entries of one user, oldest first.
    pub fn for_user(&self, user: UserId) -> Vec<&Entry> {
        self.entries.iter().filter(|e| e.user == user).collect()
    }

    /// How many kept entries carry this action.
    pub fn count(&self, action: &str) -> usize {
        self.entries.iter().filter(|e| e.action == action).count()
    }

    /// One line per entry: `SEQ user=ID ACTION TARGET`.
    pub fn render(&self) -> String {
        self.entries.iter().map(|e| format!("{} user={} {} {}\n", e.seq, e.user, e.action, e.target)).collect()
    }
}
