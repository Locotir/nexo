//! The person's standing instructions: which lines of what they wrote read as a rule for the rest of
//! the session, and the rules the curator holds for it.
//!
//! A rule is kept in the person's own words, with the turn they gave it in, for as long as the session
//! lasts: it is never dropped for its size, and a rewritten history (a compaction) carries it over.
//! The agent can restate one in a line beginning with `Rule:`; that line is kept the same way.

use std::sync::LazyLock;

use regex::Regex;

use crate::mechanical::MARK;

/// How the block marks a line the agent wrote itself (a `Rule:` line of its text or of a `note` call): the
/// header above it speaks of what the person asked, and a fact the agent saved is not a request to answer.
/// Without ": " inside: the parts that read the block split a line at the first one.
/// How the block marks a line the agent wrote itself (a `Rule:` line of its text or of a `note` call). The
/// header above it speaks of what the person asked for; a fact the agent saved is not a request, and a model
/// that reads it as one answers it ("yes, it already works that way") before it goes on. No ": " inside:
/// the parts that read the block split a line at the first one.
pub const OWN_NOTE: &str = "your own note (keep to it; it is not a request and needs no answer)";

/// Characters of a rule kept; a longer line is cut there and ends in "…".
pub const RULE_CHARS: usize = 600;
/// A line longer than this needs a strong cue: the weak ones are too common in a plain request.
const WEAK_CHARS: usize = 160;

const A: &str = "['’]";

/// Cues that make a line a rule wherever they stand, even one that reports a fault ("from now on…",
/// "make sure…", "a partir de ahora…").
static FIRM: LazyLock<String> = LazyLock::new(|| {
    [
        format!(r"i (?:do not|don{A}?t|dont) want"),
        format!(r"i{A}d rather|i would rather|i want you to|from now on|make sure"),
        format!(r"not allowed|forbidden|prohibited|must(?:n{A}?t| not)?"),
        r"a partir de ahora|de ahora en adelante|prohibid[oa]s?|deber[ií]as|debes|deb[eé]is|tienes que".into(),
        r"ten[eé]is que|quiero que|no quiero|no vuelvas|no volv[aá]is".into(),
        r"no (?:hagas|hag[aá]is|uses|us[eé]is|utilices|ejecutes|escribas|toques|modifiques|borres|cambies|pongas|a[nñ]adas|metas|lances|corras|subas|crees|instales|inventes|expliques|repitas|mezcles)".into(),
    ]
    .join("|")
});
/// Cues that make a line a rule wherever they stand, but are as common in a report of what goes wrong
/// ("every time I run it, it fails", "it returns None instead of an error").
static SOFT: LazyLock<String> = LazyLock::new(|| {
    [
        r"every time|whenever|no more|instead of|prefer(?:ably)?|avoid(?:ing)?".to_string(),
        r"stop \w+ing|important(?:ly)?|importante".into(),
        r"hay que|prefiero|ten en cuenta|tened en cuenta|ojo|en vez de|en lugar de".into(),
        r"cuidado con|evita\w*|deja de|dejad de".into(),
    ]
    .join("|")
});
/// Words that report a fault: a line with one is a rule only through a firm cue or a clause that opens
/// with an order ("Never swallow errors"), not through a soft or weak cue ("It only fails on CI").
static FAULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:fail(?:s|ed|ing|ures?)?|crash(?:es|ed|ing)?|errors?|errores|broken|bugs?|buggy|panic(?:s|ked|king)?|exceptions?|falla[ns]?|fallando|fall[oó]s?|fallid[oa]s?|rot[oa]s?|peta[ns]?|pet[oó]|no funciona\w*|se cuelga\w*)\b",
    )
    .expect("static pattern")
});
/// Lines that change or withdraw an earlier instruction ("forget the tabs rule", "ya no hace falta…"):
/// kept as rules, after the one they change, so the latest word is in the block. Matched on [`fold`].
static WITHDRAW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:forget (?:about )?(?:the|that|my|what|this|those|these|all|it)\b|scratch that|never ?mind|disregard (?:the|that|my|what|this)|ignore (?:the|that|my|this) (?:\w+ )?(?:rule|instruction)s?|no longer (?:need|have|has|must|required|necessary|appl(?:y|ies))|(?:it'?s|it is) (?:fine|ok|okay|allowed) (?:now )?to|feel free to|olvida(?:te|d)? (?:de|lo|la|el|los|las|eso|esa|ese|aquello)\b|olvidate\b|ya no (?:hace falta|es necesario|tienes que|hay que)|no hace falta que|puedes volver a|ya se puede)",
    )
    .expect("static pattern")
});
/// "You can now…", "you can use it again", "ya puedes…": a permission that lifts an earlier rule, unless its
/// verb only points at something ("you can see the error in the log"). Matched on [`fold`].
static CAN_NOW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:(?:you|we) (?:can|may|are allowed to)|(?P<ya>ya) (?:puedes|podeis|se puede)) (?P<pre>(?:now|again) )?(?P<verb>[\w']+)(?P<post>[^.!?]*\b(?:now|again|de nuevo|otra vez)\b)?")
        .expect("static pattern")
});
/// Verbs after "you can" that point at something rather than allow it ("you can see…", "ya puedes probarlo").
const POINTING: &[&str] = &[
    "see",
    "find",
    "read",
    "look",
    "check",
    "tell",
    "notice",
    "view",
    "watch",
    "try",
    "test",
    "run",
    "open",
    "start",
    "continue",
    "go",
    "proceed",
    "ver",
    "verlo",
    "mirar",
    "mirarlo",
    "leer",
    "encontrar",
    "comprobar",
    "comprobarlo",
    "probar",
    "probarlo",
    "seguir",
    "continuar",
    "empezar",
    "abrir",
    "ejecutar",
    "lanzar",
];
/// Whether ``line`` changes or withdraws an earlier instruction.
fn withdraws(line: &str) -> bool {
    let f = fold(line);
    WITHDRAW.is_match(&f)
        || CAN_NOW.captures_iter(&f).any(|c| {
            let verb = c.name("verb").map_or("", |m| m.as_str());
            let timed = c.name("ya").is_some() || c.name("pre").is_some() || c.name("post").is_some();
            timed && !POINTING.contains(&verb)
        })
}
/// Cues that are a rule only at the head of a clause: "Always run the tests", not "it always fails".
const LEAD: &str = r"never|always|remember|nunca|siempre|jam[aá]s|recu[eé]rd\w*|recordad|recordar|usa|utiliza|escribe";
/// "Do not": a rule at the head of a clause, after a head word or after "you" ("and don't push", not "we don't have CI").
const NEG: &str = r"do not|don['’]?t|dont";
/// Cues that count only in a short line ("only the tests", "keep it short", "sin comentarios").
const WEAK: &str =
    r"only|keep|without|rules?|use\b[^.;!?]{1,60}?\b(?:not|instead)|solo|s[oó]lo|[uú]nicamente|sin|solamente";

static CUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b(?:(?P<strong>{}|{})|(?P<lead>{LEAD})|(?P<neg>{NEG})|(?P<weak>{WEAK}))\b",
        *FIRM, *SOFT
    ))
    .expect("static pattern")
});
static FIRM_CUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\b(?:{})\b", *FIRM)).expect("static pattern"));
/// Words that open an order at the head of a clause, besides the task verbs and the lead cues.
const ORDER_WORDS: &[&str] = &["don't", "dont", "use", "keep", "make", "avoid", "prefer", "stop", "evita", "deja"];
static LEAD_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("^(?:{LEAD})$")).expect("static pattern"));
/// Fault verbs in the bare form an order gives them ("never panic", "don't crash", "no falles").
const BARE_FAULTS: &[&str] = &["fail", "crash", "panic", "break", "error", "falles", "petes", "rompas"];
/// Words a clause may open with before its order ("so never push", "por favor, usa…").
const CLAUSE_OPENERS: &[&str] = &[
    "please", "pls", "so", "then", "also", "just", "now", "ok", "okay", "vale", "ahora", "por", "favor", "entonces",
    "luego",
];
/// Words after an opening verb that make it a subject: "Build fails", "Test is broken", "Prueba falla".
const NOT_AFTER_ORDER: &[&str] = &[
    "is",
    "was",
    "are",
    "were",
    "has",
    "had",
    "keeps",
    "kept",
    "still",
    "sometimes",
    "randomly",
    "only",
    "step",
    "job",
    "script",
    "output",
    "log",
    "process",
    "es",
    "esta",
    "sigue",
    "solo",
];

/// Whether a clause of ``line`` (between punctuation) opens with an order: a lead cue ("never",
/// "siempre"), "don't", a task verb or another imperative, after any opener words.
fn a_clause_opens_with_an_order(line: &str) -> bool {
    let f = fold(line);
    f.split([',', '.', ';', ':', '!', '(', ')', '"']).any(|clause| {
        let words: Vec<&str> = clause
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''))
            .filter(|w| !w.is_empty())
            .skip_while(|w| CLAUSE_OPENERS.contains(w))
            .collect();
        let Some(&first) = words.first() else { return false };
        let next = words.get(1).copied().unwrap_or("");
        if next == "not" && first == "do" {
            return true;
        }
        // after "never", "always" or "don't" a fault verb in its bare form is the order ("Never panic",
        // "Don't fail silently"); an inflected one is a report ("Always fails on CI")
        let leads = LEAD_WORD.is_match(first) || first == "don't" || first == "dont";
        if leads && BARE_FAULTS.contains(&next) {
            return true;
        }
        let verb = TASK_VERBS.contains(&first) || ORDER_WORDS.contains(&first) || LEAD_WORD.is_match(first);
        verb && !FAULT.is_match(next) && !NOT_AFTER_ORDER.contains(&next)
    })
}

/// Verbs that open a task request ("Implement…", "Ahora ejecuta…"): what follows is the task, and a
/// "must" or "make sure" in it is about the task, not a rule for the rest of the session.
const TASK_VERBS: &[&str] = &[
    "implement",
    "add",
    "fix",
    "run",
    "create",
    "write",
    "build",
    "refactor",
    "update",
    "change",
    "rename",
    "move",
    "delete",
    "remove",
    "port",
    "test",
    "check",
    "look",
    "open",
    "read",
    "show",
    "find",
    "explain",
    "review",
    "debug",
    "install",
    "set",
    "deploy",
    "commit",
    "merge",
    "rewrite",
    "finish",
    "continue",
    "start",
    "try",
    "do",
    "let's",
    "lets",
    "go",
    "implementa",
    "anade",
    "arregla",
    "corrige",
    "ejecuta",
    "lanza",
    "crea",
    "haz",
    "cambia",
    "renombra",
    "mueve",
    "borra",
    "elimina",
    "quita",
    "revisa",
    "comprueba",
    "mira",
    "abre",
    "lee",
    "explica",
    "instala",
    "termina",
    "sigue",
    "continua",
    "prueba",
    "vamos",
    "pon",
    "actualiza",
    "refactoriza",
];
/// Words a request may open with before its verb ("Now run…", "Please fix…", "Vale, ahora haz…").
const OPENERS: &[&str] = &[
    "now", "please", "pls", "ok", "okay", "then", "next", "also", "and", "so", "first", "finally", "ahora", "vale",
    "por", "favor", "luego", "despues", "y", "primero", "bien", "tambien",
];
/// Cues that hold beyond the task they stand in: a task request with one of them still gives a rule.
static STANDING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:never|always|from now on|every time|whenever|no more|do not|don['’]?t|dont|nunca|siempre|jam[aá]s|a partir de ahora|de ahora en adelante|cada vez|no (?:hagas|uses|toques|vuelvas|quiero))\b",
    )
    .expect("static pattern")
});

/// Whether ``line`` opens as a task request: an imperative task verb, after any opener words.
fn opens_as_task(line: &str) -> bool {
    let f = fold(line);
    let mut words = f
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''))
        .filter(|w| !w.is_empty());
    words.find(|w| !OPENERS.contains(w)).is_some_and(|w| TASK_VERBS.contains(&w))
}

/// Words after which "never", "always", "remember" and the Spanish imperatives open a clause.
const HEAD_WORDS: &[&str] = &[
    "you", "you'll", "please", "pls", "and", "but", "so", "then", "also", "should", "must", "will", "to", "just", "we",
    "we'll", "por", "favor", "y", "e", "o", "or", "pero", "que", "tambien", "también", "ahora", "now",
];
/// Lead cues that are also a plain third person present ("usa", "escribe"): see [`is_rule`].
const AMBIGUOUS_LEADS: &[&str] = &["usa", "utiliza", "escribe"];
/// Words after which "do not" is an order: "and don't push", "you don't push" (not "we don't have CI").
const NEG_HEADS: &[&str] = &[
    "you", "please", "pls", "and", "but", "so", "then", "also", "just", "now", "or", "ok", "okay", "y", "o", "pero",
    "vale", "ahora",
];
/// Words after "do not" that make it a description ("don't know", "don't understand").
const NOT_ORDERS: &[&str] = &["know", "understand", "see", "get", "think", "care", "worry", "mind"];
/// Replies that only acknowledge.
const ACKS: &[&str] = &[
    "ok",
    "okay",
    "okey",
    "k",
    "vale",
    "si",
    "yes",
    "yep",
    "yeah",
    "no",
    "thanks",
    "thank you",
    "thx",
    "ty",
    "gracias",
    "muchas gracias",
    "perfect",
    "perfecto",
    "great",
    "genial",
    "good",
    "bien",
    "muy bien",
    "ok thanks",
    "ok gracias",
    "vale gracias",
    "sounds good",
    "de acuerdo",
    "claro",
    "continue",
    "continua",
    "sigue",
    "go on",
    "go ahead",
    "dale",
    "adelante",
    "done",
    "listo",
    "nice",
    "cool",
];

/// `text` without accents, lower case.
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            '’' => '\'',
            c => c,
        })
        .collect()
}

/// What two wordings of one rule share: no case, accents, list marker, outer punctuation or extra space.
pub fn key(text: &str) -> String {
    let f = fold(text);
    let words: Vec<&str> = f.split_whitespace().collect();
    let joined = words.join(" ");
    joined
        .trim_start_matches(|c: char| matches!(c, '-' | '*' | '•' | '>' | '¡' | '¿') || c.is_whitespace())
        .trim_end_matches(|c: char| matches!(c, '.' | '!' | ';' | ',' | ':' | '…') || c.is_whitespace())
        .to_string()
}

fn last_word(before: &str) -> &str {
    before.split_whitespace().next_back().unwrap_or("")
}

/// Whether the person's ``line`` reads as a standing instruction.
pub fn is_rule(line: &str) -> bool {
    let line = line.trim();
    let k = key(line);
    if k.is_empty() || ACKS.contains(&k.as_str()) || line.starts_with('>') || line.starts_with("//") {
        return false;
    }
    // a question asks; a line of code or of a log describes
    if line.ends_with('?') || line.starts_with('¿') || line.ends_with(['{', '}', ';']) {
        return false;
    }
    // a line that changes or withdraws an earlier instruction: kept, so the latest word is in the block
    if withdraws(line) {
        return true;
    }
    // a plain task request: its "must" and "make sure" are about the task
    if opens_as_task(line) && !STANDING.is_match(line) {
        return false;
    }
    // a report of a fault: only a firm cue or a clause that opens with an order makes it a rule
    if FAULT.is_match(line) {
        return FIRM_CUE.is_match(line) || a_clause_opens_with_an_order(line);
    }
    let short = line.chars().count() <= WEAK_CHARS;
    for c in CUE.captures_iter(line) {
        let m = c.get(0).expect("whole match");
        let before = line[..m.start()].trim_end();
        let prev = fold(last_word(before));
        let prev = prev.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'');
        let clause_head =
            before.is_empty() || before.ends_with([',', '.', ';', ':', '!', '¡', '-', '–', '—', '(', '*', '"', '\'']);
        if c.name("strong").is_some() {
            return true;
        }
        // a Spanish imperative that reads the same as the third person ("usa", "escribe") is an order only at
        // the head of a clause or after a polite opener: after "y" or "que" it describes ("el build lee x y
        // escribe y")
        let heads: &[&str] = if c.name("lead").is_some_and(|l| AMBIGUOUS_LEADS.contains(&fold(l.as_str()).as_str())) {
            CLAUSE_OPENERS
        } else {
            HEAD_WORDS
        };
        if c.name("lead").is_some() && (clause_head || heads.contains(&prev)) {
            return true;
        }
        if c.name("neg").is_some() {
            let next = fold(line[m.end()..].split_whitespace().next().unwrap_or(""));
            if (clause_head || NEG_HEADS.contains(&prev)) && !NOT_ORDERS.contains(&next.as_str()) {
                return true;
            }
        }
        if c.name("weak").is_some() && short {
            return true;
        }
    }
    false
}

/// ``line`` as kept: trimmed, and cut at [`RULE_CHARS`].
pub fn clip(line: &str) -> String {
    let line = line.trim();
    match line.char_indices().nth(RULE_CHARS - 1) {
        Some((i, _)) if line[i..].chars().nth(1).is_some() => format!("{}…", &line[..i]),
        _ => line.to_string(),
    }
}

/// The sentences of ``line``: each ends at ".", "!", "?" or ";" followed by a space.
fn sentences(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | ';') && chars.peek().is_some_and(|(_, n)| n.is_whitespace()) {
            out.push(line[from..=i].trim());
            from = i + 1;
        }
    }
    out.push(line[from..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// A line that opens a block a host puts in the person's message (`<system-reminder>`, `<command-output>`):
/// its tag name.
static TAG_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^<([A-Za-z][\w:.-]*)(?:\s[^>]*)?>").expect("static pattern"));
/// A line that opens as a log line: a level, a compiler message, a timestamp or a bracketed level.
static LOG_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:\[?(?:ERROR|WARN|WARNING|INFO|DEBUG|FATAL|TRACE|CRITICAL)\b|(?:error|warning)(?:\[\w+\])?:|\[(?i:error|warn|warning|info|debug|fatal|trace)\]|\[?\d{4}-\d{2}-\d{2}|\[?\d{1,2}:\d{2}:\d{2})",
    )
    .expect("static pattern")
});

/// The rules in the person's ``text``, code blocks, blocks a host put in it and log lines left out: each
/// line that reads as one, or, in a long line or one with a task request in it, the sentences that do.
pub fn rules_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut code = false;
    // the tag of the host block the line is in
    let mut block: Option<String> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            code = !code;
            continue;
        }
        if let Some(tag) = &block {
            if t.contains(&format!("</{tag}>")) {
                block = None;
            }
            continue;
        }
        if code {
            continue;
        }
        if let Some(c) = TAG_OPEN.captures(t) {
            let tag = &c[1];
            // a host's tag is a compound name (`system-reminder`, `user_instructions`); a plain one (`<p>`,
            // `<br>`) is the person's own markup and opens no block that would hide the rest of the message,
            // nor does a self-closing one (`<router-view />`)
            if tag.contains(['-', '_']) && !c[0].ends_with("/>") && !t.contains(&format!("</{tag}>")) {
                block = Some(tag.to_string());
            }
            continue;
        }
        if t.starts_with('<') || LOG_LINE.is_match(t) {
            continue;
        }
        let all = sentences(line);
        let task = all.iter().any(|s| opens_as_task(s));
        if !(task || is_rule(line)) {
            continue;
        }
        let parts: Vec<&str> = all.iter().copied().filter(|s| is_rule(s)).collect();
        if !task && (line.trim().chars().count() <= WEAK_CHARS || parts.is_empty()) {
            out.push(clip(line));
        } else {
            out.extend(parts.into_iter().map(clip));
        }
    }
    out
}

/// One rule: the person's words (or the agent's restatement), the turn and message it came in.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub turn: usize,
    /// The message it is in; `None` once the history it came in was rewritten.
    pub msg: Option<usize>,
    pub text: String,
    pub restated: bool,
    key: String,
}

/// The rules of a session, in the order they were given.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub list: Vec<Rule>,
    /// The person's turns so far.
    pub turn: usize,
    /// Set while a rewritten history is read anew: the rules found in it are the carried ones, in place.
    pub replay: bool,
}

impl Rules {
    /// Adds a rule; one with the same wording is kept once (the first, in the message that has it now).
    /// Given again after other rules in a new message, it moves to the end with its new turn: it is the
    /// latest word over whatever changed it in between. Found again while a rewritten history is read
    /// anew (`replay`), a carried rule is only tied to its message, in place.
    pub fn add(&mut self, msg: usize, text: String, restated: bool) {
        let k = key(&text);
        if let Some(i) = self.list.iter().position(|r| r.key == k) {
            // said by the person as well, it is the person's: never shown as the agent's own note
            self.list[i].restated &= restated;
            let last = i + 1 == self.list.len();
            let was = self.list[i].msg;
            if last || was == Some(msg) || (was.is_none() && self.replay) {
                self.list[i].msg.get_or_insert(msg);
            } else {
                let mut r = self.list.remove(i);
                (r.turn, r.msg) = (self.turn.max(1), Some(msg));
                self.list.push(r);
            }
            return;
        }
        self.list.push(Rule { turn: self.turn.max(1), msg: Some(msg), text, restated, key: k });
    }

    /// The rules for a rewritten history: all of them, no longer tied to a message, and the turns
    /// counting on from the last one.
    pub fn carried(self) -> Rules {
        let turn = self.list.iter().map(|r| r.turn).max().unwrap_or(0);
        let list = self.list.into_iter().map(|r| Rule { msg: None, ..r }).collect();
        Rules { list, turn, replay: false }
    }

    /// The block of the rules ``shown`` takes, or "" when it takes none.
    pub fn text(&self, shown: impl Fn(&Rule) -> bool) -> String {
        let mut lines = vec![format!(
            "{MARK} Standing instructions the user gave during this session. They hold until the user changes them; on conflict the later one wins. A task among them was asked in its turn, not in the current request:"
        )];
        for r in self.list.iter().filter(|r| shown(r)) {
            if r.restated {
                lines.push(format!("turn {}, {OWN_NOTE}: {}", r.turn, r.text));
            } else {
                lines.push(format!("turn {}: {}", r.turn, r.text));
            }
        }
        if lines.len() == 1 {
            return String::new();
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_instructions_are_rules() {
        for l in [
            "Never use unwrap in library code.",
            "always run cargo fmt before you commit",
            "Please don't touch the tests.",
            "Do not add new dependencies",
            "From now on, answer in English.",
            "Remember to update the changelog",
            "I want you to keep the public API stable",
            "I don't want any emoji in the output",
            "I'd rather you used tabs",
            "Use anyhow, not Box<dyn Error>",
            "Make sure every function has a doc comment",
            "Only touch files under src/",
            "Keep the replies short",
            "Commits must be signed",
            "Avoid global state",
            "Prefer iterators over index loops",
            "Whenever you change the schema, bump the version",
            "No more println debugging",
            "Stop adding comments to every line",
        ] {
            assert!(is_rule(l), "{l}");
        }
    }

    #[test]
    fn spanish_instructions_are_rules() {
        for l in [
            "Nunca uses unwrap",
            "siempre responde en español",
            "Jamás borres la carpeta de datos",
            "Recuérdalo: los tests no se tocan",
            "recuerda pasar clippy",
            "A partir de ahora escribe los commits en inglés",
            "Debes usar rustfmt",
            "tienes que comentar en español",
            "No hagas push sin preguntar",
            "no uses println",
            "No quiero dependencias nuevas",
            "No vuelvas a tocar el README",
            "Usa tabs, no espacios",
            "Evita los clones innecesarios",
            "Deja de añadir comentarios",
            "Quiero que los nombres sean en inglés",
            "Prefiero funciones cortas",
            "Ten en cuenta que el servidor es Windows",
            "Ojo con los permisos",
            "Importante: el código en inglés",
            "En vez de println usa tracing",
            "Solo cambia el fichero main.rs",
            "Únicamente en la rama dev",
            "Cuidado con los acentos",
            "prohibido usar unsafe",
        ] {
            assert!(is_rule(l), "{l}");
        }
    }

    #[test]
    fn questions_requests_and_acks_are_not_rules() {
        for l in [
            "ok",
            "Sí.",
            "gracias!",
            "Vale",
            "yes",
            "Thanks",
            "Can you always use tabs?",
            "¿Por qué nunca pasa el test?",
            "Fix the failing test in parser.rs",
            "Arregla el test que falla en parser.rs",
            "I don't know why it fails",
            "The test always fails on Windows",
            "el test siempre falla",
            "I've never seen this error before",
            "> never use this function",
            "let x = never_used();",
            "",
        ] {
            assert!(!is_rule(l), "{l}");
        }
    }

    #[test]
    fn a_weak_cue_needs_a_short_line() {
        assert!(is_rule("Keep the replies short"));
        let long = format!("Keep {} going", "the build log that the job printed and ".repeat(5));
        assert!(!is_rule(&long));
        let strong = format!("Never {} again", "touch the build log that the job printed and ".repeat(5));
        assert!(is_rule(&strong));
    }

    #[test]
    fn code_blocks_are_not_read_for_rules() {
        let text = "Here is the log:\n```\nnever returns\nalways = 1\n```\nNever use unwrap.";
        assert_eq!(rules_in(text), vec!["Never use unwrap.".to_string()]);
    }

    #[test]
    fn a_long_request_gives_only_its_rule_sentences() {
        let line = "Now run the audit tool and fix every problem it reports, until it exits with code 0 and nothing \
                    else is printed on the console. Can you also check the logs? The tests must still pass.";
        assert_eq!(rules_in(line), vec!["The tests must still pass.".to_string()]);
        assert_eq!(rules_in("Fix the bug. Never push."), vec!["Never push.".to_string()]);
    }

    #[test]
    fn a_plain_task_request_is_not_a_rule() {
        for l in [
            "Implement docs/spec/01-restock.md. Add tests for it and make sure all the tests pass.",
            "Add a test for the parser and make sure it passes",
            "Now run the linter and fix every problem it reports, without editing its config",
            "Ahora ejecuta los tests y asegúrate de que pasan, solo los de integración",
            "Implementa la función y tiene que compilar",
        ] {
            assert!(rules_in(l).is_empty(), "{l}");
        }
        let l = "Implement docs/spec/02-reserve.md, with tests in a new file; all the tests must pass.";
        assert_eq!(rules_in(l), vec!["all the tests must pass.".to_string()]);
        assert!(is_rule("Fix the parser and never touch the lexer"));
        assert!(is_rule("Ahora corrige el parser y no toques el lexer"));
        assert!(is_rule("Escribe los commits en inglés"));
    }

    #[test]
    fn a_long_rule_is_cut_not_dropped() {
        let line = format!("Never {}", "x".repeat(800));
        let r = rules_in(&line);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].chars().count(), RULE_CHARS);
        assert!(r[0].ends_with('…'));
        assert_eq!(clip("Never do that"), "Never do that");
    }

    #[test]
    fn the_same_rule_twice_is_kept_once() {
        let mut rs = Rules { turn: 1, ..Default::default() };
        rs.add(0, "Never use unwrap.".into(), false);
        rs.turn = 4;
        rs.add(9, "never  use unwrap".into(), false);
        rs.add(9, "Nunca uses unwrap".into(), false);
        assert_eq!(rs.list.len(), 2);
        assert_eq!((rs.list[0].turn, rs.list[0].msg), (1, Some(0)));
    }

    #[test]
    fn a_rule_the_person_gives_after_the_agent_noted_it_is_the_persons() {
        // in place (it is the last one) and moved to the end (another rule came in between)
        for between in [false, true] {
            let mut rs = Rules { turn: 1, ..Default::default() };
            rs.add(1, "Use four spaces.".into(), true);
            assert!(rs.text(|_| true).contains(&format!("turn 1, {OWN_NOTE}: Use four spaces.")));
            if between {
                rs.add(2, "Never delete the logs.".into(), false);
            }
            rs.turn = 3;
            rs.add(5, "use four spaces".into(), false);
            let t = rs.text(|_| true);
            assert!(!t.contains(OWN_NOTE), "{t}");
            assert!(t.contains(&format!("turn {}: Use four spaces.", if between { 3 } else { 1 })), "{t}");
            // and the agent noting it again does not take it back
            rs.add(7, "Use four spaces.".into(), true);
            assert!(!rs.text(|_| true).contains(OWN_NOTE));
        }
    }

    #[test]
    fn rules_are_listed_in_the_order_given() {
        let mut rs = Rules { turn: 2, ..Default::default() };
        rs.add(3, "Always use tabs".into(), false);
        rs.turn = 7;
        rs.add(20, "Use spaces, not tabs".into(), false);
        rs.add(21, "Use four spaces.".into(), true);
        let t = rs.text(|_| true);
        let (a, b) = (t.find("turn 2: Always use tabs").unwrap(), t.find("turn 7: Use spaces, not tabs").unwrap());
        assert!(a < b, "{t}");
        assert!(t.contains(&format!("turn 7, {OWN_NOTE}: Use four spaces.")));
        assert!(!OWN_NOTE.contains(": "));
        assert!(t.contains("the later one wins"));
        assert_eq!(rs.text(|_| false), "");
    }

    #[test]
    fn a_rewritten_history_carries_every_rule() {
        let mut rs = Rules { turn: 3, ..Default::default() };
        rs.add(5, "Never push".into(), false);
        let mut c = rs.carried();
        assert_eq!(c.list[0].msg, None);
        assert_eq!(c.turn, 3);
        c.add(1, "never push".into(), false);
        assert_eq!((c.list.len(), c.list[0].msg, c.list[0].turn), (1, Some(1), 3), "found again: tied anew");
    }

    #[test]
    fn a_carried_rule_given_again_in_a_new_message_moves_past_what_withdrew_it() {
        let mut rs = Rules { turn: 2, ..Default::default() };
        rs.add(3, "Never push to main.".into(), false);
        rs.add(5, "You may push to main from now on.".into(), false);
        let mut c = rs.clone().carried();
        // the rewritten history is read anew: both are found where they were
        c.replay = true;
        c.add(1, "Never push to main.".into(), false);
        c.add(2, "You may push to main from now on.".into(), false);
        assert_eq!(c.list[0].text, "Never push to main.");
        // the history the harness rewrote dropped the first; the person gives it again
        let mut c = rs.carried();
        c.replay = true;
        c.add(2, "You may push to main from now on.".into(), false);
        c.replay = false;
        c.turn = 5;
        c.add(9, "Never push to main.".into(), false);
        assert_eq!((c.list[1].text.as_str(), c.list[1].turn, c.list[1].msg), ("Never push to main.", 5, Some(9)));
    }

    #[test]
    fn a_fault_report_is_not_a_rule_without_a_firm_cue_or_an_order() {
        for l in [
            "The parser returns None instead of an error",
            "Every time I run the tests they fail",
            "Whenever I open it, it crashes",
            "This is an important bug",
            "no more errors now",
            "It only fails on CI",
            "El test falla sin motivo",
            "Solo falla en Windows",
            "the lint rules fail",
            "It crashes, but never on Linux",
            "On Linux, build fails every time",
            "Always fails on CI",
            "Nunca falla en Linux",
        ] {
            assert!(!is_rule(l), "{l}");
        }
        for l in [
            "Whenever a test fails, fix it before moving on",
            "Never swallow errors",
            "Always fix failing tests first",
            "Don't commit code with errors",
            "Make sure the build has no errors",
            "Use tabs instead of spaces",
            "Every time you touch the parser, run its tests",
            "The build is broken, so never push to main",
            "Nunca ignores los errores",
            "Si falla un test, no hagas commit",
            "Never panic in library code",
            "Don't fail silently",
            "Always crash loudly on bad input",
        ] {
            assert!(is_rule(l), "{l}");
        }
    }

    #[test]
    fn do_not_after_a_subject_describes() {
        for l in ["We don't have CI yet", "Users don't see the button", "The tests do not cover it"] {
            assert!(!is_rule(l), "{l}");
        }
        for l in ["Don't use unwrap", "Please don't touch main", "and don't push to main", "you don't push to main"] {
            assert!(is_rule(l), "{l}");
        }
    }

    #[test]
    fn a_line_that_withdraws_a_rule_is_kept_as_one() {
        for l in [
            "Forget the tabs rule",
            "you can use unwrap now",
            "You can now push to main",
            "ya puedes usar unwrap",
            "olvida lo de los tabs",
            "ya no hace falta que pases clippy",
            "You no longer need to sign the commits",
            "scratch that",
        ] {
            assert!(is_rule(l), "{l}");
        }
        for l in ["you can see the error in the log", "you can see the error in the log now", "ya puedes probarlo"] {
            assert!(!is_rule(l), "{l}");
        }
    }

    #[test]
    fn a_rule_given_again_after_others_moves_to_the_end() {
        let mut rs = Rules { turn: 2, ..Default::default() };
        rs.add(3, "Never use unwrap.".into(), false);
        rs.turn = 5;
        rs.add(9, "you can use unwrap now".into(), false);
        rs.turn = 8;
        rs.add(15, "never use unwrap".into(), false);
        let t = rs.text(|_| true);
        assert!(
            t.find("turn 5: you can use unwrap now").unwrap() < t.find("turn 8: Never use unwrap.").unwrap(),
            "{t}"
        );
        assert_eq!(rs.list.len(), 2);
        // carried over a rewrite and found again in a summary: it keeps its place
        let mut c = rs.carried();
        c.turn = 9;
        c.replay = true;
        c.add(1, "you can use unwrap now".into(), false);
        assert_eq!((c.list[0].turn, c.list[1].turn, c.list[0].msg), (5, 8, Some(1)), "{:?}", c.list);
    }

    #[test]
    fn host_blocks_and_log_lines_give_no_rules() {
        let text = "<system-reminder>\nNever use tabs.\nYou must answer in French.\n</system-reminder>\n\
                    <command-output>Always run the linter</command-output>\n\
                    ERROR: not allowed to write /etc/x\n\
                    2026-11-04 10:00:01 must restart the worker\n\
                    [INFO] make sure the cache is warm\n\
                    error: you must not borrow twice\n\
                    warning: never used variable\n\
                    Warning: never push on Fridays.";
        assert_eq!(rules_in(text), vec!["Warning: never push on Fridays.".to_string()]);
        // the person's own markup opens no block: the line after it still counts
        assert_eq!(rules_in("<p>\nNever use tabs."), vec!["Never use tabs.".to_string()]);
        // nor does a self-closing component, in a code block or out of it
        let text = "In App.vue I have:\n```vue\n<router-view />\n```\n<el-button />\nNever use the options API.";
        assert_eq!(rules_in(text), vec!["Never use the options API.".to_string()]);
    }

    #[test]
    fn a_spanish_verb_after_y_or_que_describes() {
        for l in ["El build lee config.toml y escribe el resultado en dist/", "El servidor que usa nginx va lento"] {
            assert!(!is_rule(l), "{l}");
        }
        for l in ["Usa tabs", "Por favor, usa tabs", "por favor usa tabs", "Ahora escribe los tests en inglés"] {
            assert!(is_rule(l), "{l}");
        }
    }
}
