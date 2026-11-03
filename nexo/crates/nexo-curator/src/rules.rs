//! The person's standing instructions: which lines of what they wrote read as a rule for the rest of
//! the session, and the rules the curator holds for it.
//!
//! A rule is kept in the person's own words, with the turn they gave it in, for as long as the session
//! lasts: it is never dropped for its size, and a rewritten history (a compaction) carries it over.
//! The agent can restate one in a line beginning with `Rule:`; that line is kept the same way.

use std::sync::LazyLock;

use regex::Regex;

use crate::mechanical::MARK;

/// Characters of a rule kept; a longer line is cut there and ends in "…".
pub const RULE_CHARS: usize = 600;
/// A line longer than this needs a strong cue: the weak ones are too common in a plain request.
const WEAK_CHARS: usize = 160;

const A: &str = "['’]";

/// Cues that make a line a rule wherever they stand ("I'd rather…", "a partir de ahora…").
static STRONG: LazyLock<String> = LazyLock::new(|| {
    [
        format!(r"i (?:do not|don{A}?t|dont) want"),
        format!(r"i{A}d rather|i would rather|i want you to|from now on|make sure|every time|whenever|no more"),
        format!(r"not allowed|forbidden|prohibited|must(?:n{A}?t| not)?|instead of|prefer(?:ably)?|avoid(?:ing)?"),
        r"stop \w+ing|important(?:ly)?|importante".into(),
        r"a partir de ahora|de ahora en adelante|prohibid[oa]s?|deber[ií]as|debes|deb[eé]is|tienes que".into(),
        r"ten[eé]is que|hay que|quiero que|prefiero|ten en cuenta|tened en cuenta|ojo|en vez de|en lugar de".into(),
        r"cuidado con|evita\w*|deja de|dejad de|no quiero|no vuelvas|no volv[aá]is".into(),
        r"no (?:hagas|hag[aá]is|uses|us[eé]is|utilices|ejecutes|escribas|toques|modifiques|borres|cambies|pongas|a[nñ]adas|metas|lances|corras|subas|crees|instales|inventes|expliques|repitas|mezcles)".into(),
    ]
    .join("|")
});
/// Cues that are a rule only at the head of a clause: "Always run the tests", not "it always fails".
const LEAD: &str = r"never|always|remember|nunca|siempre|jam[aá]s|recu[eé]rd\w*|recordad|recordar|usa|utiliza|escribe";
/// "Do not": a rule unless what comes before says it describes something ("I don't know").
const NEG: &str = r"do not|don['’]?t|dont";
/// Cues that count only in a short line ("only the tests", "keep it short", "sin comentarios").
const WEAK: &str =
    r"only|keep|without|rules?|use\b[^.;!?]{1,60}?\b(?:not|instead)|solo|s[oó]lo|[uú]nicamente|sin|solamente";

static CUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b(?:(?P<strong>{})|(?P<lead>{LEAD})|(?P<neg>{NEG})|(?P<weak>{WEAK}))\b", *STRONG))
        .expect("static pattern")
});

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
/// Words before "do not" that make it a description, not an instruction ("I don't know", "it doesn't").
const DESCRIBING: &[&str] =
    &["i", "it", "this", "that", "he", "she", "they", "which", "who", "there", "what", "why", "it's", "i'm", "these"];
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
    // a plain task request: its "must" and "make sure" are about the task
    if opens_as_task(line) && !STANDING.is_match(line) {
        return false;
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
        if c.name("lead").is_some() && (clause_head || HEAD_WORDS.contains(&prev)) {
            return true;
        }
        if c.name("neg").is_some() {
            let next = fold(line[m.end()..].split_whitespace().next().unwrap_or(""));
            if !DESCRIBING.contains(&prev) && !NOT_ORDERS.contains(&next.as_str()) {
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

/// The rules in the person's ``text``, code blocks left out: each line that reads as one, or, in a long
/// line or one with a task request in it, the sentences that do.
pub fn rules_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        let all = sentences(line);
        let task = all.iter().any(|s| opens_as_task(s));
        if code || !(task || is_rule(line)) {
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
}

impl Rules {
    /// Adds a rule; one with the same wording is kept once (the first, in the message that has it now).
    pub fn add(&mut self, msg: usize, text: String, restated: bool) {
        let k = key(&text);
        if let Some(r) = self.list.iter_mut().find(|r| r.key == k) {
            if r.msg.is_none() {
                r.msg = Some(msg);
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
        Rules { list, turn }
    }

    /// The block of the rules ``shown`` takes, or "" when it takes none.
    pub fn text(&self, shown: impl Fn(&Rule) -> bool) -> String {
        let mut lines = vec![format!(
            "{MARK} Standing instructions the user gave during this session. They hold until the user changes them; on conflict the later one wins. Follow them in every step, as if they were part of the current request:"
        )];
        for r in self.list.iter().filter(|r| shown(r)) {
            if r.restated {
                lines.push(format!("turn {}, as you restated it: {}", r.turn, r.text));
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
    fn rules_are_listed_in_the_order_given() {
        let mut rs = Rules { turn: 2, ..Default::default() };
        rs.add(3, "Always use tabs".into(), false);
        rs.turn = 7;
        rs.add(20, "Use spaces, not tabs".into(), false);
        rs.add(21, "Use four spaces.".into(), true);
        let t = rs.text(|_| true);
        let (a, b) = (t.find("turn 2: Always use tabs").unwrap(), t.find("turn 7: Use spaces, not tabs").unwrap());
        assert!(a < b, "{t}");
        assert!(t.contains("turn 7, as you restated it: Use four spaces."));
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
}
