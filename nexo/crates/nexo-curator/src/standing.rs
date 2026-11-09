//! Standing facts: what the session learnt that overrides a default (a shell rc that is not at `~/.bashrc`, a
//! database on another port, a folder not to touch), kept by target.
//!
//! A fact is a line of the session's own words (the person's, the agent's `Finding:`, or an assignment a tool
//! printed: `shell_rc = /etc/skel/rc`), taken as it arrives, with the targets it governs: the stems of its
//! words, the base name of its paths (`~/.bashrc`, `/home/x/.bashrc` and `bashrc` are one) and its ports.
//! When the person's request, the agent's last words or the inputs of its last calls hold one of those targets,
//! the fact is due: it goes into the tail of the view although nobody asked for it, because the action about
//! to be made supposes the default and never names the fact. The table is small and never drops a fact for
//! budget; only a later fact about the same subject replaces it (the new line shows the old one as `antes:`).
//! No list of domains or tools: the targets are the words themselves.

use std::sync::LazyLock;

use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

/// Facts kept; past it the oldest go.
const TABLE: usize = 200;
/// Facts taken from one message.
const PER_MESSAGE: usize = 4;
/// Facts taken from one tool output (a config file may hold many assignments).
const PER_RESULT: usize = 8;
/// Characters of a message read for facts.
const READ_CHARS: usize = 6000;
/// A sentence longer than this is not a fact: it is prose or a paste.
const LINE_CHARS: usize = 240;
/// Characters of each end of the person's request and of the agent's last text that make the trigger.
const TRIGGER_CHARS: usize = 300;
/// Requests before a word that most of them hold stops being a target.
const COMMON_AFTER: usize = 50;
/// Characters of the inputs of the agent's calls that make the trigger.
const CALL_CHARS: usize = 600;

/// The inputs of a call that name what it acts on.
pub const CALL_KEYS: &[&str] =
    &["command", "cmd", "path", "file_path", "filepath", "file", "url", "pattern", "query", "dir", "cwd"];

/// Words that govern nothing.
const STOP: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "if", "then", "of", "to", "in", "on", "at", "by", "for", "with", "from",
    "as", "is", "are", "was", "were", "be", "been", "it", "its", "this", "that", "these", "those", "i", "you", "we",
    "they", "he", "she", "me", "my", "your", "our", "their", "not", "no", "do", "does", "did", "don", "t", "s", "can",
    "could", "should", "would", "will", "just", "so", "than", "into", "onto", "up", "out", "about", "have", "has",
    "had", "there", "here", "which", "who", "what", "when", "where", "how", "all", "any", "some", "more", "most",
    "also", "only", "very", "too", "lives", "live", "listens", "stored", "kept", "saved", "located", "instead",
    "rather", "never", "always", "el", "la", "los", "las", "un", "una", "unos", "unas", "de", "del", "y", "o", "en",
    "con", "por", "para", "que", "se", "es", "son", "mi", "tu", "su", "al", "lo", "est", "esta", "estan",
];

/// A negation with the alternative it replaces: "rg, not grep", "not at ~/.bashrc", "instead of", "en vez de".
static CONTRAST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:,\s*not\s+\S|\bnot\s+(?:at|in|on|under|from|~|/|\./|\.\w)|\b(?:instead of|rather than|en vez de|en lugar de)\b|\bno est[aá]n?\s+en\b|,\s*no\s+(?:en|~|/))",
    )
    .expect("static pattern")
});
/// A prohibition: "don't touch", "never edit", "read-only", "no toques".
static PROHIBIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\b(?:don['’]?t|do not|never|must not|mustn['’]?t|nunca|no)\s+(?:touch|edit|modify|change|delete|remove|write|overwrite|commit|push|run|use|read|open|access|mess with|toques?|edites?|modifiques?|borres?|cambies?|escribas?|uses?)\b|\bread-only\b|\bsolo lectura\b|\boff-limits\b)",
    )
    .expect("static pattern")
});
/// Where something is, said as a fact about it: "lives at", "is stored in", "listens on", "se guarda en".
static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\b(?:lives?|located|stored|kept|saved|sits|listens|runs|is|are)\s+(?:at|in|on|under)\b|\bse (?:guarda|encuentra)n?\s+en\b|\best[aá]n?\s+en\b)",
    )
    .expect("static pattern")
});
/// Something concrete a location fact points to: a path, a host:port, a port.
static CONCRETE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:[/~]|:\d{2,5}\b|\bport\s*\d{2,5}\b)").expect("static pattern"));
/// A preference: "prefer", "always use", "use X for".
static PREFER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:prefer(?:ably)?|always use|only use|use \S+ for|prefiero|usa \S+ para)\b")
        .expect("static pattern")
});
/// An assignment a tool printed: `shell_rc = /etc/skel/rc`, `export TOKEN_FILE=~/.secrets/t`.
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*(?:export\s+)?([A-Za-z_][\w.-]*)\s*=\s*["']?((?:/|~/)[^\s"']+)["']?\s*$"#)
        .expect("static pattern")
});
/// A port named in a text: `port 5433`, `:8797`, `-p 5433`, the alternative of a "not 5432".
static PORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\bport\s*[:=]?\s*|:|\s-p\s*|\bnot\s+)(\d{2,5})\b").expect("static pattern"));
/// The line of a note that carries a finding.
static FINDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?mi)^[ \t]*(?:[-*•][ \t]*|\d+[.)][ \t]*)?(?:\*\*)?Finding(?:\*\*)?:(?:\*\*)?[ \t]*(.+?)[ \t]*$")
        .expect("static pattern")
});
/// Where a subject ends: the first verb of the statement or the first sign.
static SUBJECT_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:\b(?:is|are|isn['’]t|lives?|listens|stored|kept|saved|goes|runs|located|sits|se|est[aá]n?)\b|[=:,(])",
    )
    .expect("static pattern")
});

/// The stem of a word: a plural "s" off (when long enough and not "ss"), cut to five characters.
fn stem(w: &str) -> String {
    let n = w.chars().count();
    let w = if n >= 4 && w.ends_with('s') && !w.ends_with("ss") { &w[..w.len() - 1] } else { w };
    w.chars().take(5).collect()
}

/// The stems that name what ``text`` is about, in order and without repeats. A word with a `/` is a path:
/// only its last part counts (`~/.bashrc`, `/home/x/.bashrc` and `bashrc` are the same target).
pub fn stems(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = word.trim_end_matches(|c: char| !c.is_alphanumeric());
        let part = if word.contains('/') { word.rsplit('/').find(|p| !p.is_empty()).unwrap_or("") } else { word };
        for tok in part.split(|c: char| !c.is_alphanumeric()) {
            let tok = tok.to_lowercase();
            if tok.chars().count() < 2 || tok.chars().all(|c| c.is_ascii_digit()) || STOP.contains(&tok.as_str()) {
                continue;
            }
            let s = stem(&tok);
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    for c in PORT.captures_iter(text) {
        let p = format!("#{}", &c[1]);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// The stems of the subject of a statement: what comes before its verb ("the staging server is at…").
fn subject(line: &str) -> Vec<String> {
    let cut = SUBJECT_END.find(line).map_or(line.len(), |m| m.start());
    let s = stems(&line[..cut]);
    if s.is_empty() {
        stems(line)
    } else {
        s
    }
}

/// The sentences of a line: each ends at ".", "!", "?" or ";" followed by a space or the end.
fn sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | ';') && chars.peek().is_none_or(|(_, n)| n.is_whitespace()) {
            let end = if c == ';' { i } else { i + c.len_utf8() };
            out.push(&line[from..end]);
            from = i + c.len_utf8();
        }
    }
    out.push(&line[from..]);
    out
}

/// Whether a statement is a standing fact: it denies a default and gives the alternative, says where something
/// is, forbids, or prefers.
fn is_fact(s: &str) -> bool {
    CONTRAST.is_match(s) || PROHIBIT.is_match(s) || PREFER.is_match(s) || (LOCATION.is_match(s) && CONCRETE.is_match(s))
}

/// The head and the tail of ``s``, ``n`` characters each.
fn ends(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len <= 2 * n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    let tail: String = s.chars().skip(len - n).collect();
    format!("{head}\n{tail}")
}

#[derive(Clone, Debug)]
struct Fact {
    /// The line in the session's words.
    line: String,
    targets: Vec<String>,
    subject: Vec<String>,
    /// The message it came from (`None`: from before the history was rewritten).
    src: Option<usize>,
    /// The line this one replaced.
    before: Option<String>,
}

/// The table of standing facts, and what the trigger reads.
#[derive(Default)]
pub struct Standing {
    facts: Vec<Fact>,
    /// Text turns of the person so far, and in how many of them each stem appears.
    requests: usize,
    df: FxHashMap<String, usize>,
    /// The person's last text turn, the agent's last text and the inputs of its last calls.
    user: String,
    said: String,
    calls: String,
}

impl Standing {
    /// The facts only (a rewritten history is read anew; the facts it no longer holds stay, from no message).
    pub fn carried(self) -> Standing {
        let facts = self.facts.into_iter().map(|f| Fact { src: None, ..f }).collect();
        Standing { facts, ..Standing::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// The messages the facts came from.
    pub fn sources(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.facts.iter().filter_map(|f| f.src).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Takes in message ``mi``: ``user`` says whether the person sent it (a tool result comes as one), ``texts`` is
    /// its text (the agent's, with its notes), ``results`` the outputs it carries and ``calls`` the inputs of
    /// its calls that name something.
    pub fn see(&mut self, mi: usize, user: bool, texts: &[String], results: &[String], calls: &[String]) {
        let text = texts.join("\n");
        let text: String = text.chars().take(READ_CHARS).collect();
        let mut taken = 0;
        if user {
            if !text.trim().is_empty() {
                self.requests += 1;
                for s in stems(&text) {
                    *self.df.entry(s).or_insert(0) += 1;
                }
                self.user = ends(&text, TRIGGER_CHARS);
                for line in text.lines().filter(|l| !l.trim_start().starts_with("```")) {
                    for s in sentences(line) {
                        if taken < PER_MESSAGE && self.take(mi, s) {
                            taken += 1;
                        }
                    }
                }
            }
            let mut from_results = 0;
            for r in results {
                for line in r.lines().filter(|l| l.len() <= LINE_CHARS && l.contains('=')) {
                    if from_results >= PER_RESULT {
                        break;
                    }
                    if let Some(c) = ASSIGN.captures(line) {
                        let mut targets = stems(line);
                        targets.retain(|t| !t.starts_with('#'));
                        if self.put(mi, line.trim().to_string(), targets, stems(&c[1])) {
                            from_results += 1;
                        }
                    }
                }
            }
        } else {
            self.said = ends(&text, TRIGGER_CHARS);
            self.calls = calls.join("\n").chars().take(CALL_CHARS).collect();
            for c in FINDING.captures_iter(&text) {
                for s in sentences(&c[1]) {
                    if taken < PER_MESSAGE && self.take(mi, s) {
                        taken += 1;
                    }
                }
            }
        }
    }

    /// Takes a sentence if it is a fact.
    fn take(&mut self, mi: usize, s: &str) -> bool {
        let s = s.trim().trim_start_matches(['-', '*', '•', '>', '#', ' ']);
        if s.chars().count() < 12 || s.chars().count() > LINE_CHARS || !is_fact(s) {
            return false;
        }
        self.put(mi, s.to_string(), stems(s), subject(s))
    }

    /// Adds a fact; the same line again only updates where it came from, and a later line about the same
    /// subject replaces the old one.
    fn put(&mut self, mi: usize, line: String, targets: Vec<String>, subject: Vec<String>) -> bool {
        if targets.is_empty() {
            return false;
        }
        if let Some(f) = self.facts.iter_mut().find(|f| f.line == line) {
            f.src = Some(mi);
            return false;
        }
        let mut before = None;
        if let Some(k) = self.facts.iter().position(|f| f.subject == subject) {
            // the fact the old one replaced, read again after a rewrite: the table holds the newer one
            if self.facts[k].before.as_deref() == Some(line.as_str()) {
                return false;
            }
            before = Some(self.facts.remove(k).line);
        }
        self.facts.push(Fact { line, targets, subject, src: Some(mi), before });
        if self.facts.len() > TABLE {
            self.facts.remove(0);
        }
        true
    }

    /// Whether most requests hold ``stem``: it says nothing about any of them.
    fn common(&self, stem: &str) -> bool {
        self.requests >= COMMON_AFTER && self.df.get(stem).is_some_and(|d| d * 2 > self.requests)
    }

    /// The facts due now, as lines for the tail: those with a target the person's request, the agent's last
    /// text or the inputs of its last calls hold, the most targets first, the newest first among equals.
    /// ``skip`` says which are already in the view (their message is whole in it, or the rules block has them).
    pub fn due(&self, limit: usize, skip: &dyn Fn(Option<usize>, &str) -> bool) -> Vec<String> {
        if self.facts.is_empty() || limit == 0 {
            return Vec::new();
        }
        let trigger: FxHashSet<String> = [&self.user, &self.said, &self.calls]
            .into_iter()
            .flat_map(|t| stems(t))
            .filter(|s| !self.common(s))
            .collect();
        let mut hit: Vec<(usize, usize, &Fact)> = self
            .facts
            .iter()
            .enumerate()
            .filter(|(_, f)| !skip(f.src, &f.line))
            .filter_map(|(k, f)| {
                let n = f.targets.iter().filter(|t| trigger.contains(*t)).count();
                (n > 0).then_some((n, k, f))
            })
            .collect();
        hit.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        hit.into_iter()
            .take(limit)
            .map(|(_, _, f)| match &f.before {
                Some(old) => format!("{} (antes: {old})", f.line),
                None => f.line.clone(),
            })
            .collect()
    }
}
