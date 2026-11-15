//! The distinctive words of a text and the tally of the ones a reply forgot.

use crate::hindsight;

/// The distinctive words of a text (names, paths, long numbers): the ones the curator follows from
/// the material that holds them to the replies that copy them.
pub fn distinctive_words(text: &str) -> Vec<String> {
    let mut int = hindsight::Interner::default();
    hindsight::words(text, &mut int).into_iter().map(|w| int.get(w).to_string()).collect()
}

/// What a distinctive word is, for a report on what was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordKind {
    /// Only digits and the signs of a number (`4373.25`, `1,372`, `-8500.0`).
    Number,
    /// An identifier, path, version or hash: it has a `/`, `\`, `_`, `-`, a dot inside, or a digit among
    /// letters (`tools/audit.py`, `SPEC_EXAMPLES`, `sulko_dolde1`, `40114b`).
    Identifier,
    /// Anything else: letters only (a plain name).
    Other,
}

/// The kind of a word from [`distinctive_words`]. Mechanical, no dictionary: the rule of the H4 notes
/// (a digit, slash or inner dot makes a word private; a number is split off) without the word list.
pub fn word_kind(word: &str) -> WordKind {
    let w = word.trim_matches(|c: char| !c.is_alphanumeric());
    if w.chars().any(|c| c.is_ascii_digit())
        && w.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+'))
        && !w.contains("--")
    {
        return WordKind::Number;
    }
    // a hash: hex digits only, long enough not to be a word
    if w.len() >= 6 && w.chars().all(|c| c.is_ascii_hexdigit()) {
        return WordKind::Identifier;
    }
    let joined = w.chars().any(|c| matches!(c, '/' | '\\' | '_' | '-' | '.') || c.is_ascii_digit());
    if joined {
        WordKind::Identifier
    } else {
        WordKind::Other
    }
}

/// What the replies forgot, by kind and by reply. The plain total (`words`) is the number the earlier
/// tables use; the rest says what the loss is made of and whether one reply holds most of it.
#[derive(Debug, Default, Clone)]
pub struct LostTally {
    /// Forgotten words in all, a word once per reply it is forgotten in.
    pub words: usize,
    pub numbers: usize,
    pub identifiers: usize,
    pub other: usize,
    /// Distinct forgotten words over the whole conversation: one forgotten in many replies counts once.
    distinct: std::collections::HashSet<String>,
    /// The reply with the most forgotten words: its label and count.
    pub largest: Option<(usize, usize)>,
    /// Replies that forgot something.
    pub replies: usize,
}

impl LostTally {
    /// Counts the words forgotten in reply number `reply`.
    pub fn add(&mut self, reply: usize, lost: &[String]) {
        if lost.is_empty() {
            return;
        }
        self.replies += 1;
        self.words += lost.len();
        for w in lost {
            match word_kind(w) {
                WordKind::Number => self.numbers += 1,
                WordKind::Identifier => self.identifiers += 1,
                WordKind::Other => self.other += 1,
            }
            self.distinct.insert(w.clone());
        }
        if self.largest.is_none_or(|(_, n)| lost.len() > n) {
            self.largest = Some((reply, lost.len()));
        }
    }

    /// Distinct forgotten words over the conversation.
    pub fn distinct(&self) -> usize {
        self.distinct.len()
    }

    /// Forgotten words outside the largest reply: what the loss is when one table does not dominate.
    pub fn without_largest(&self) -> usize {
        self.words - self.largest.map_or(0, |(_, n)| n)
    }

    /// The share, in percent, of all forgotten words that the largest reply holds.
    pub fn share_of_largest(&self) -> f64 {
        match self.largest {
            Some((_, n)) if self.words > 0 => 100.0 * n as f64 / self.words as f64,
            _ => 0.0,
        }
    }
}
