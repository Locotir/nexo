//! Filler words removed from the prose of user messages; code and quoted text untouched (`compress`).

use std::borrow::Cow;
use std::sync::LazyLock;

use rustc_hash::FxHashSet;

use crate::py::{self, Re, S};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    En,
    Es,
}

fn set(words: &'static str) -> FxHashSet<&'static str> {
    words.split_whitespace().collect()
}

/// Fillers dropped wherever they stand (unless glued to `_ . / -`).
static DROP_EN: LazyLock<FxHashSet<&'static str>> =
    LazyLock::new(|| set("the is are was were just really actually basically simply literally please kindly um uh"));
static DROP_ES: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("el la los las un una unos unas pues digamos eh porfa básicamente simplemente literalmente realmente")
});
/// Interjections dropped only at a clause start when a comma, stop or the end follows.
static OPENERS: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| set("okay ok hey hi hello vale okey bueno"));
/// Words an article never stands before: after one of these, `a` / `an` is a name, not an article.
static EN_FUNCTION: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("to of in on at by for from with and or but nor yet is are was were be been am the a an this that these \
         those it its as if then than so not no into onto over under after before via per vs versus do does did \
         has have had will would can could should may might must shall i you he she we they me him her us them \
         my your our their his which what who whom whose where when why how there here all any each every some \
         both either neither also too very just")
});
/// Words after which `sort of` / `kind of` is a noun phrase.
static DETERMINERS: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("a an the this that these those some any each every one another what which no my your our their his \
         her its un una unos unas el la los las este esta ese esa algún alguna ningún ninguna cada otro otra")
});
/// Common function words used to tell the language of a message.
static MARK_EN: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("the and is are to of it this that with for you i in on not what how be have do does can will my we \
         they should would from but if was there please")
});
static MARK_ES: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("el la los las y es que de del al por para con se lo una un pero más como cómo qué está esto este esta \
         hay porque muy ya también yo eso cuando donde sí en")
});
static MARK_OTHER: LazyLock<FxHashSet<&'static str>> = LazyLock::new(|| {
    set("le les et est il des du je nous vous pas avec pour sur dans ce qui une au aux ne elle mais sont que la \
         c'est l'on di che è non per gli della sono questo anche ma ho nel alla uno ci un una \
         o os não do da em você é um com para isso ao ele eu das dos \
         els amb és aquest aquesta això però perquè també molt bé \
         unha cun coa isto iso moi tamén polo pola")
});

/// Phrases, longest first; each is a filler only where its rule says so.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// Anywhere.
    Always,
    /// Followed by a comma, a stop or the end, and not right after another word.
    Tag,
    /// At a clause start and followed by a comma.
    Opener,
    /// Followed by a comma, a stop or the end.
    Trailing,
    /// Not after a determiner.
    NoDeterminer,
}
const PHRASES: [(Lang, &str, Rule); 16] = [
    (Lang::Es, "la verdad es que", Rule::Always),
    (Lang::Es, "o algo así", Rule::Always),
    (Lang::Es, "me entiendes", Rule::Tag),
    (Lang::En, "you know", Rule::Tag),
    (Lang::En, "i mean", Rule::Opener),
    (Lang::En, "kind of", Rule::NoDeterminer),
    (Lang::En, "sort of", Rule::NoDeterminer),
    (Lang::En, "let's say", Rule::Opener),
    (Lang::Es, "o sea", Rule::Always),
    (Lang::Es, "en plan", Rule::Trailing),
    (Lang::Es, "la verdad", Rule::Opener),
    (Lang::Es, "por favor", Rule::Always),
    (Lang::Es, "o algo", Rule::Trailing),
    (Lang::Es, "sabes", Rule::Tag),
    (Lang::Es, "entiendes", Rule::Tag),
    (Lang::Es, "comprendes", Rule::Tag),
];

static PROTECT: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        concat!(
            r"```.*?(?:```|\Z)",
            r"|`[^`\n]+`",
            r"|<[^<>\n]{1,200}>",
            r#"|"[^"\n]{1,300}"|“[^”\n]{1,300}”"#,
            r"|https?://\S+",
            r"|(?m:^(?:    |\t)[^\n]*$)",
            r"|(?m:^[^\n]*[{};=<>\[\]|\\$][^\n]*$)"
        ),
        S,
    )
});
static WORDISH: LazyLock<Re> = LazyLock::new(|| Re::new(r"[^\W\d_](?:[^\W\d_]|['’][^\W\d_])*", 0));
static SPACES: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]{2,}", 0));
static BEFORE_PUNCT: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]+([,.;:!?)])", 0));
static AFTER_OPEN: LazyLock<Re> = LazyLock::new(|| Re::new(r"([(¿¡]) +", 0));
static COMMAS: LazyLock<Re> = LazyLock::new(|| Re::new(r",\s*,", 0));
static INDENT: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n[ \t]+(?=\S)", 0));
static TRAILING: LazyLock<Re> = LazyLock::new(|| Re::new(r"[ \t]+\n", 0));
static BLANKS: LazyLock<Re> = LazyLock::new(|| Re::new(r"\n{3,}", 0));

fn glued(b: u8) -> bool {
    matches!(b, b'_' | b'.' | b'/' | b'-')
}

fn is_blank(s: &str) -> bool {
    s.bytes().all(|c| c == b' ' || c == b'\t')
}

/// The language of `text` when it clearly reads as English or Spanish.
fn lang(text: &str) -> Option<Lang> {
    let (mut en, mut es, mut other) = (0usize, 0usize, 0usize);
    for (a, b) in WORDISH.find_all(text) {
        let w = py::lower(&text[a..b]);
        let w = w.as_ref();
        en += usize::from(MARK_EN.contains(w));
        es += usize::from(MARK_ES.contains(w));
        other += usize::from(MARK_OTHER.contains(w));
    }
    let pick = |win: usize, rest: usize| win >= 2 && win > rest;
    if pick(en, es.max(other)) {
        Some(Lang::En)
    } else if pick(es, en.max(other)) {
        Some(Lang::Es)
    } else {
        None
    }
}

struct Tok {
    a: usize,
    b: usize,
    low: String,
}

fn prose(text: &str, lang: Lang) -> String {
    let lead = if text.is_empty() || text.starts_with([' ', '\t']) { " " } else { "" };
    let tail = if text.is_empty() || text.ends_with([' ', '\t']) { " " } else { "" };
    let bytes = text.as_bytes();
    let toks: Vec<Tok> = WORDISH
        .find_all(text)
        .into_iter()
        .map(|(a, b)| Tok { a, b, low: py::lower(&text[a..b]).into_owned() })
        .collect();
    let prev_char = |a: usize| text[..a].trim_end_matches([' ', '\t']).chars().last();
    let next_char = |b: usize| text[b..].trim_start_matches([' ', '\t']).chars().next();
    let clause_start = |a: usize| prev_char(a).is_none_or(|c| ".!?¿¡,;:\n(".contains(c));
    let stop_after = |b: usize| next_char(b).is_none_or(|c| ",.!?;:\n)".contains(c));
    // The word right before token `i`, when only blanks sit between them.
    let prev_word = |i: usize| (i > 0 && is_blank(&text[toks[i - 1].b..toks[i].a])).then(|| toks[i - 1].low.as_str());
    let next_word =
        |j: usize| (j + 1 < toks.len() && is_blank(&text[toks[j].b..toks[j + 1].a])).then(|| toks[j + 1].low.as_str());
    let is_glued = |a: usize, b: usize| (a > 0 && glued(bytes[a - 1])) || (b < bytes.len() && glued(bytes[b]));
    // A comma right after `b` (past blanks): the end of that comma.
    let comma_after = |b: usize| {
        let rest = text[b..].trim_start_matches([' ', '\t']);
        rest.starts_with(',').then(|| text.len() - rest.len() + 1)
    };
    // Tag removal: `, ¿sabes?` goes whole, marks and leading comma included.
    let tag_span = |a: usize, b: usize| {
        let rest = text[b..].trim_start_matches([' ', '\t']);
        if !rest.starts_with('?') {
            return (a, b);
        }
        let end = text.len() - rest.len() + 1;
        let mut start = text[..a].trim_end_matches([' ', '\t']);
        start = start.strip_suffix('¿').unwrap_or(start).trim_end_matches([' ', '\t']);
        start = start.strip_suffix(',').unwrap_or(start);
        (start.len(), end)
    };

    let mut cut: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    'tokens: while i < toks.len() {
        for &(l, phrase, rule) in &PHRASES {
            if l != lang {
                continue;
            }
            let n = phrase.split(' ').count();
            if i + n > toks.len() || !phrase.split(' ').zip(&toks[i..i + n]).all(|(p, t)| p == t.low) {
                continue;
            }
            let j = i + n - 1;
            if (1..n).any(|k| !is_blank(&text[toks[i + k - 1].b..toks[i + k].a])) {
                continue;
            }
            let (a, b) = (toks[i].a, toks[j].b);
            if is_glued(a, b) {
                continue;
            }
            let span = match rule {
                Rule::Always => Some((a, b)),
                Rule::Tag => (prev_word(i).is_none() && stop_after(b)).then(|| tag_span(a, b)),
                Rule::Opener => (clause_start(a) && comma_after(b).is_some()).then(|| (a, comma_after(b).unwrap())),
                Rule::Trailing => stop_after(b).then_some((a, b)),
                Rule::NoDeterminer => (!prev_word(i).is_some_and(|w| DETERMINERS.contains(w))).then_some((a, b)),
            };
            if let Some(s) = span {
                cut.push(s);
                i = j + 1;
                continue 'tokens;
            }
        }
        let (a, b, w) = (toks[i].a, toks[i].b, toks[i].low.as_str());
        let drop = !is_glued(a, b)
            && match lang {
                Lang::En => {
                    DROP_EN.contains(w)
                        || (matches!(w, "a" | "an")
                            && next_word(i).is_some_and(|n| py::len(n) > 1 && !EN_FUNCTION.contains(n)))
                        || (w == "am" && prev_word(i) == Some("i"))
                }
                Lang::Es => DROP_ES.contains(w),
            };
        if drop {
            cut.push((a, b));
        } else if OPENERS.contains(w) && !is_glued(a, b) && clause_start(a) && stop_after(b) {
            cut.push((a, comma_after(b).unwrap_or(b)));
        }
        i += 1;
    }

    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (a, b) in cut {
        if a > pos {
            out.push_str(&text[pos..a]);
        }
        pos = pos.max(b);
    }
    out.push_str(&text[pos.min(text.len())..]);
    let text = SPACES.sub(&out, " ").into_owned();
    let text = BEFORE_PUNCT.sub(&text, r"\1").into_owned();
    let text = AFTER_OPEN.sub(&text, r"\1").into_owned();
    let text = COMMAS.sub(&text, ",").into_owned();
    let text = INDENT.sub(&text, "\n").into_owned();
    let text = TRAILING.sub(&text, "\n").into_owned();
    let core = text.trim_matches([' ', '\t']);
    if core.is_empty() {
        if lead.is_empty() { tail } else { lead }.to_string()
    } else {
        format!("{lead}{core}{tail}")
    }
}

/// `text` with filler words removed from its prose. Only English or Spanish
/// prose is touched; other languages, and text whose language is unclear, stay as they are.
pub fn compress(text: &str) -> Cow<'_, str> {
    if text.is_empty() || py::strip(text).is_empty() {
        return Cow::Borrowed(text);
    }
    let kept = PROTECT.find_all(text);
    let mut plain = String::with_capacity(text.len());
    let mut pos = 0;
    for &(a, b) in &kept {
        plain.push_str(&text[pos..a]);
        plain.push(' ');
        pos = b;
    }
    plain.push_str(&text[pos..]);
    let Some(lang) = lang(&plain) else { return Cow::Borrowed(text) };
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    for (a, b) in kept {
        if a > pos {
            out.push_str(&prose(&text[pos..a], lang));
        }
        out.push_str(&text[a..b]);
        pos = b;
    }
    if pos < text.len() {
        out.push_str(&prose(&text[pos..], lang));
    }
    let res = BLANKS.sub(&out, "\n\n").into_owned();
    if py::len(&res) < py::len(text) {
        Cow::Owned(res)
    } else {
        Cow::Borrowed(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_filler() {
        assert_eq!(compress("Fix the failing test in src/lib.rs"), "Fix failing test in src/lib.rs");
        assert_eq!(compress("please, you know, run `the tests`"), ", run `the tests`");
    }

    #[test]
    fn keeps_one_letter_identifier() {
        assert_eq!(compress("rename a to b in the parser"), "rename a to b in parser");
        assert_eq!(compress("add a test for the parser"), "add test for parser");
    }

    #[test]
    fn keeps_fillers_that_carry_meaning() {
        let es = |s| compress(s).into_owned();
        assert_eq!(
            es("Ahora voy a borrarlo porque el test no sirve para nada"),
            "Ahora voy a borrarlo porque test no sirve para nada"
        );
        assert_eq!(es("¿Cuánto vale la licencia para el equipo?"), "¿Cuánto vale licencia para equipo?");
        assert_eq!(es("Vale, pues lo hago yo con el script"), "lo hago yo con script");
        assert_eq!(es("Dime la verdad sobre el error de la build"), "Dime verdad sobre error de build");
        assert_eq!(es("La verdad es que el test falla por el cache"), "test falla por cache");
        assert_eq!(es("¿Sabes cómo se hace el despliegue?"), "¿Sabes cómo se hace despliegue?");
        assert_eq!(es("Esto es lo que hay con el script, ¿sabes?"), "Esto es lo que hay con script");
        assert_eq!(compress("The meeting is at 9 am in the main room"), "meeting at 9 am in main room");
        assert_eq!(compress("I am fixing the bug in the parser"), "I fixing bug in parser");
        assert_eq!(compress("do a sort of the keys and then print them"), "do sort of keys and then print them");
        assert_eq!(compress("it is sort of slow on the server"), "it slow on server");
        assert_eq!(compress("do you know the answer to this?"), "do you know answer to this?");
    }

    #[test]
    fn other_languages_untouched() {
        for s in [
            "il a dit que la fonction est cassée dans le code",
            "il codice è una bella soluzione per il problema",
            "o código não está a funcionar com a base de dados",
            "els fitxers de configuració no es carreguen bé amb la versió nova",
            "o ficheiro de configuración non se carga ben coa versión nova, e isto é un problema",
        ] {
            assert_eq!(compress(s), s);
        }
    }

    #[test]
    fn code_paths_numbers_quotes_untouched() {
        assert_eq!(
            compress("Please run the tests in `the a b` and \"the value\" at /tmp/the/a.txt for version 1.2.3"),
            "run tests in `the a b` and \"the value\" at /tmp/the/a.txt for version 1.2.3"
        );
        let block = "```\nthe a = basically;\n```";
        assert_eq!(
            compress(&format!("Look at the code in this file:\n{block}")),
            format!("Look at code in this file:\n{block}")
        );
    }
}
