//! Long-context correlation tasks. Two read Don Quixote (Project Gutenberg #2000, fetched once at
//! setup and cached, never committed): the answer is a few quotes that must exist verbatim in the
//! chapter the answer names. One reads a small fictional web app and documents the request its own
//! front end sends, facts spread over pages, script, middleware and settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

use super::{always, Task};
use crate::check::{read, Outcome, Part};
use crate::llmb::sha::hex;

/// The long2 tasks.
pub fn tasks() -> Vec<Task> {
    vec![
        quixote_windmills(),
        closed(quixote_windmills(), "quixote-windmills-closed"),
        quixote_island(),
        closed(quixote_island(), "quixote-island-closed"),
        quixote_thread(),
        closed(quixote_thread(), "quixote-insula-thread-closed"),
        share_request(),
    ]
}

// ---------------------------------------------------------------- the book

const BOOK_URL: &str = "https://www.gutenberg.org/cache/epub/2000/pg2000.txt";
const BOOK_SHA256: &str = "8a3738c61aa507f2319fad526a886c2de74c17c7ba1d64726c53d3591fa62321";

fn cache_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join("build-cache/bench-data/pg2000.txt"))
}

fn cached_ok(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| hex(&b) == BOOK_SHA256)
}

/// The raw book, from the cache or downloaded once; the SHA-256 must match.
static DOWNLOAD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn book_raw() -> Result<String, String> {
    let _one_at_a_time = DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner());
    let path = cache_file().ok_or("no HOME")?;
    if !cached_ok(&path) {
        let dir = path.parent().ok_or("no cache folder")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let tmp = path.with_extension(format!("part{}", std::process::id()));
        let out = Command::new("curl")
            .args(["-sSL", "-m", "180", "-o"])
            .arg(&tmp)
            .arg(BOOK_URL)
            .output()
            .map_err(|e| format!("cannot run curl: {e}"))?;
        if !out.status.success() {
            return Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        if !cached_ok(&tmp) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("the downloaded book does not match SHA-256 {BOOK_SHA256}"));
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).replace('\r', ""))
}

fn ready_book() -> Result<(), String> {
    if cache_file().is_some_and(|p| cached_ok(&p)) {
        return Ok(());
    }
    match Command::new("curl").arg("--version").output() {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err("needs curl (or the cached book in ~/build-cache/bench-data) for the Don Quixote text".into()),
    }
}

// ---------------------------------------------------------------- the perturbed book

/// Seed of the perturbation: an edit with several variants draws its variant from a generator
/// seeded with this number and the edit's id, so the book is the same on every machine and run.
const SEED: u64 = 0x5155_494a_4f54_4532;

/// One edit of the book. `find` occurs exactly once in the Gutenberg text. A swap replaces it with
/// a variant; an insertion keeps it and appends the variant, a sentence in the book's register that
/// no real edition has.
struct Edit {
    id: &'static str,
    find: &'static str,
    insert: bool,
    variants: &'static [&'static str],
}

/// Every edit, in the order they apply: three numbers or names that tasks ask for, one name in
/// the thread of the island, three inserted sentences about the island. Few and plausible; the
/// rest of the text is byte-identical to Gutenberg. Questions depend on them, so an answer recalled
/// from the real book is wrong where it matters.
const EDITS: [Edit; 7] = [
    Edit {
        id: "windmills",
        find: "treinta o cuarenta molinos de viento",
        insert: false,
        variants: &[
            "veinte o veinticinco molinos de viento",
            "doce o quince molinos de viento",
            "diez o doce molinos de viento",
        ],
    },
    Edit {
        id: "kingdom",
        find: "el reino de Dinamarca o el de Soliadisa",
        insert: false,
        variants: &[
            "el reino de Noruega o el de Soliadisa",
            "el reino de Bohemia o el de Soliadisa",
            "el reino de Escocia o el de Soliadisa",
        ],
    },
    Edit {
        id: "town",
        find: "mil vecinos, que era",
        insert: false,
        variants: &["quinientos vecinos, que era", "setecientos vecinos, que era", "ochocientos vecinos, que era"],
    },
    Edit {
        id: "isle",
        find: "conde fue de la ínsula Firme",
        insert: false,
        variants: &["conde fue de la ínsula Fría", "conde fue de la ínsula Dorada"],
    },
    Edit {
        id: "count",
        find: "tan trabados y desesperados negocios.",
        insert: true,
        variants: &[" Sancho, entre tanto, no hacía sino contar con los dedos los días que le faltaban para ser \
                     gobernador, y decía a media voz que si había de ser ínsula de tierra firme o de agua cercada, \
                     que lo mismo le daba, con tal que tuviese puerta por donde entrar y salir con su jumento."],
    },
    Edit {
        id: "boat",
        find: "por la mesma mentira.",
        insert: true,
        variants: &[
            " Sancho, por su parte, no pensaba sino en la ínsula que su amo le tenía prometida, y \
                     refunfuñaba que más quería un cortijo pequeño en tierra firme que una ínsula grande en las nubes.",
        ],
    },
    Edit {
        id: "wood",
        find: "aunque hay pocos que le parezcan.",
        insert: true,
        variants: &[" Sancho, que escuchaba desde lo apartado, no hacía sino rogar por la salud de su amo, porque sin él se le \
                     acababa la esperanza de la ínsula."],
    },
];

/// The text an edit puts in the book (the variant its seed draws), in full.
fn variant(id: &str) -> &'static str {
    let e = EDITS.iter().find(|e| e.id == id).expect("a known edit");
    let h = id.bytes().fold(SEED, |h, b| h.wrapping_mul(0x100_0000_01b3) ^ u64::from(b));
    crate::rng::Rng::new(h).pick::<&str>(e.variants)
}

/// The real book with the edits applied. Fails if an anchor is not found exactly once.
fn perturb(raw: &str) -> Result<String, String> {
    let mut out = raw.to_string();
    for e in &EDITS {
        let n = out.matches(e.find).count();
        if n != 1 {
            return Err(format!("edit {}: {:?} occurs {n} times in the book, not once", e.id, e.find));
        }
        let v = variant(e.id);
        out = out.replacen(e.find, &if e.insert { format!("{}{v}", e.find) } else { v.to_string() }, 1);
    }
    Ok(out)
}

/// The perturbed book, the one every grader and every working directory uses.
fn book_text() -> Result<String, String> {
    perturb(&book_raw()?)
}

fn setup_book(wd: &Path) -> Result<(), String> {
    std::fs::write(wd.join("quijote.txt"), book_text()?).map_err(|e| e.to_string())
}

/// Lowercase, accents dropped, everything but letters and digits a single space.
fn norm(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = match c {
            'á' | 'à' | 'ä' => 'a',
            'é' | 'è' | 'ë' => 'e',
            'í' | 'ì' | 'ï' => 'i',
            'ó' | 'ò' | 'ö' => 'o',
            'ú' | 'ù' | 'ü' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn roman(s: &str) -> Option<u32> {
    let s = s.to_lowercase();
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    if matches!(s.as_str(), "primero" | "primer" | "one") {
        return Some(1);
    }
    let val = |c| match c {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        _ => None,
    };
    let v: Option<Vec<u32>> = s.chars().map(val).collect();
    let v = v.filter(|v| !v.is_empty())?;
    let mut total = 0;
    for (i, n) in v.iter().enumerate() {
        if v.get(i + 1).is_some_and(|m| m > n) {
            total -= *n as i64;
        } else {
            total += *n as i64;
        }
    }
    u32::try_from(total).ok()
}

/// `(part, chapter)` -> normalized chapter text.
fn chapters(raw: &str) -> BTreeMap<(u32, u32), String> {
    let start = raw.find("*** START OF").unwrap_or(0);
    let end = raw.find("*** END OF").unwrap_or(raw.len());
    let head = Regex::new(r"(?i)^Capítulo (primero|[IVXLC]+)\.").expect("valid");
    let mut out: BTreeMap<(u32, u32), String> = BTreeMap::new();
    let (mut part, mut cur) = (1, None);
    let mut seen = false;
    for line in raw[start..end].lines() {
        if let Some(n) = head.captures(line).and_then(|c| roman(&c[1])) {
            if n == 1 && seen {
                part += 1;
            }
            seen = true;
            cur = Some((part, n));
        }
        if let Some(k) = cur {
            let e = out.entry(k).or_default();
            e.push_str(line);
            e.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------- citations

/// A checked citation: its place and normalized text, or why it fails.
type Placed = Result<((u32, u32), String), String>;

/// One `Part X, chapter Y: "quote"` line of an answer.
struct Cite {
    part: Option<u32>,
    chapter: Option<u32>,
    quote: String,
}

fn cites(answer: &str) -> Vec<Cite> {
    let q = Regex::new(r#"["“«]([^"”»]{12,})["”»]"#).expect("valid");
    let part_re = Regex::new(r"(?i)\bpart[e]?\s+(ii|i|one|two|1|2|primera|segunda|first|second)\b").expect("valid");
    let chap_re =
        Regex::new(r"(?i)\b(?:chapter|cap[ií]tulo|cap|ch)\b\.?\s*([ivxlc]+|\d+|primero|primer|one)\b").expect("valid");
    let compact = Regex::new(r"(?i)^\W*(ii|i)\s*[.:/]\s*(\d+|[ivxlc]+)\b").expect("valid");
    let mut out = Vec::new();
    for line in answer.lines() {
        let Some(m) = q.captures(line) else { continue };
        let label = &line[..m.get(0).map_or(0, |g| g.start())];
        let mut part = part_re.captures(label).map(|c| match c[1].to_lowercase().as_str() {
            "i" | "one" | "1" | "primera" | "first" => 1,
            _ => 2,
        });
        let mut chapter = chap_re.captures(label).and_then(|c| roman(&c[1]));
        if let Some(c) = compact.captures(label) {
            part = part.or(Some(if c[1].eq_ignore_ascii_case("i") { 1 } else { 2 }));
            chapter = chapter.or_else(|| roman(&c[2]));
        }
        out.push(Cite { part, chapter, quote: m[1].to_string() });
    }
    out
}

/// A citation that is real: labelled, long enough, and every fragment (split at an ellipsis) in the
/// chapter it names. Returns its normalized text and place, or why not.
fn valid(c: &Cite, book: &BTreeMap<(u32, u32), String>) -> Placed {
    let at = c.part.zip(c.chapter).ok_or_else(|| format!("{:?}: no part and chapter given", c.quote))?;
    let text = book.get(&at).ok_or_else(|| format!("part {} chapter {} does not exist", at.0, at.1))?;
    let whole = norm(&c.quote);
    if whole.len() < 20 {
        return Err(format!("{:?}: too short to prove anything", c.quote));
    }
    let hay = norm(text);
    for frag in c.quote.replace('…', "...").replace("[...]", "...").split("...") {
        let f = norm(frag);
        if f.len() >= 6 && !hay.contains(&f) {
            return Err(format!("{:?} is not in part {} chapter {} of the book", c.quote, at.0, at.1));
        }
    }
    Ok((at, whole))
}

/// What one piece of evidence must be: a place and a word of the quote.
struct Need {
    why: &'static str,
    places: &'static [(u32, u32)],
    words: Vec<&'static str>,
}

fn quote_parts(wd: &Path, verdict: &str, needs: &[Need], extra: &[(&str, &str)]) -> Vec<Part> {
    let raw = match book_text() {
        Ok(b) => b,
        Err(e) => return vec![(false, e)],
    };
    let book = chapters(&raw);
    let answer = read(wd, "ANSWER.md");
    if answer.trim().is_empty() {
        return vec![(false, "ANSWER.md is missing or empty".into())];
    }
    let mut parts = Vec::new();
    let v = Regex::new(verdict).expect("valid");
    parts.push((v.is_match(&answer), "the verdict line is missing or wrong".to_string()));
    for (pattern, why) in extra {
        let re = Regex::new(pattern).expect("valid");
        parts.push((re.is_match(&answer), why.to_string()));
    }
    let all = cites(&answer);
    let checked: Vec<Placed> = all.iter().map(|c| valid(c, &book)).collect();
    let bad: Vec<&String> = checked.iter().filter_map(|r| r.as_ref().err()).collect();
    parts.push((
        all.len() >= 4 && bad.is_empty(),
        match bad.first() {
            Some(e) => format!("a citation is invented or mislabelled: {e}"),
            None => format!("needs at least four citations, found {}", all.len()),
        },
    ));
    let good: Vec<&((u32, u32), String)> = checked.iter().filter_map(|r| r.as_ref().ok()).collect();
    for n in needs {
        let met =
            good.iter().any(|(at, text)| n.places.contains(at) && n.words.iter().any(|w| text.contains(&norm(w))));
        parts.push((met, format!("missing evidence: {}", n.why)));
    }
    parts
}

fn write_answer(wd: &Path, text: &str) {
    std::fs::write(wd.join("ANSWER.md"), text).expect("a writable working directory");
}

fn book_task(
    id: &'static str,
    trap: &'static str,
    ask: &str,
    check: fn(&Path) -> Outcome,
    solution: fn(&Path),
) -> Task {
    Task {
        id,
        trap,
        group: "long2",
        files: Vec::new(),
        turns: vec![(1, ask.to_string())],
        check,
        solution,
        setup: Some(setup_book),
        sealed: false,
        mask: &[],
        ready: ready_book,
    }
}

/// The closed-book control of a book task: the same question, the same grader (which still reads
/// the perturbed book), but no `quijote.txt` in the working directory. A model that scores here
/// answers from memory of the novel, so its score on the open task proves nothing about reading.
/// What memory can still earn is the part of the answer the edits do not touch.
fn closed(open: Task, id: &'static str) -> Task {
    let ask = format!(
        "NOTE: quijote.txt is NOT in this folder; there is no book to read. Answer as well as you can without it.\n\n{}",
        open.turns[0].1
    );
    Task {
        id,
        trap: "closed-book control: the same question with no book in the folder. A pass means the answer came \
               from memory of the novel (contamination), not from reading",
        group: "closed",
        turns: vec![(1, ask)],
        setup: None,
        ..open
    }
}

// ---------------------------------------------------------------- windmills

fn windmill_needs() -> Vec<Need> {
    vec![
        Need {
            why: "how many windmills they find, as this book gives it, in Part I chapter VIII",
            places: &[(1, 8)],
            words: vec![variant("windmills").trim_end_matches(" de viento")],
        },
        Need { why: "the enchanter he blames, in Part I chapter VIII", places: &[(1, 8)], words: vec!["Frestón"] },
        Need {
            why: "how his judgment was lost, in Part I chapter I",
            places: &[(1, 1)],
            words: vec!["celebro", "perder el juicio"],
        },
        Need {
            why: "a later chapter of Part II that recalls the windmills",
            places: &[(2, 3), (2, 10), (2, 17)],
            words: vec!["molinos de viento"],
        },
    ]
}

fn windmill_parts(wd: &Path) -> Vec<Part> {
    quote_parts(wd, r"(?im)^\W*verdict\W*\s*(yes|s[ií])\b", &windmill_needs(), &[])
}

fn quixote_windmills() -> Task {
    book_task(
        "quixote-windmills",
        "a 2 MB Spanish novel with two chapters numbered VIII; the proof of a delusion is spread over the first \
         chapter, the episode and a chapter hundreds of pages later, and every quote must be real; the book is \
         lightly edited, so the windmill count recalled from memory is wrong",
        "quijote.txt holds the Spanish text of Don Quixote (both parts, about 2 MB). Is Don Quixote hallucinating in \
         chapter VIII of Part One, and how can you be sure? Write ANSWER.md. First line: `VERDICT: yes` or \
         `VERDICT: no`. Then the evidence, one item per line, as `Part I, chapter VIII: \"exact quote\"`: give \
         proof from the chapter itself (include the sentence where they first see the windmills, with the number \
         of windmills as this copy of the book states it), from the chapter where the book first shows what has \
         happened to his mind, and from a chapter of Part Two that looks back on the episode. Copy every quote \
         exactly from this file (an invented quote or a wrong part or chapter number spoils the answer).",
        |wd| Outcome::share(windmill_parts(wd)),
        windmill_solution,
    )
}

fn windmill_answer() -> String {
    format!(
        "VERDICT: yes\n\n\
Part I, chapter VIII: \"descubrieron {} que hay en aquel campo\"\n\
Part I, chapter VIII: \"aquel sabio Frestón que me robó el aposento y los libros ha vuelto estos gigantes en molinos\"\n\
Part I, chapter I: \"del poco dormir y del mucho leer, se le secó el celebro, de manera que vino a perder el juicio\"\n\
Part II, chapter X: \"como se pareció cuando dijo que los molinos de viento eran gigantes\"\n",
        variant("windmills")
    )
}

fn windmill_solution(wd: &Path) {
    write_answer(wd, &windmill_answer());
}

// ---------------------------------------------------------------- island

fn island_needs() -> Vec<Need> {
    vec![
        Need { why: "the promise of an island, Part I chapter VII", places: &[(1, 7)], words: vec!["ínsula"] },
        Need {
            why: "the fallback kingdoms, as this book names them, Part I chapter X",
            places: &[(1, 10)],
            words: vec![variant("kingdom")],
        },
        Need { why: "the island he is given, Part II chapter XLV", places: &[(2, 45)], words: vec!["Barataria"] },
        Need {
            why: "the size of the town, as this book gives it, Part II chapter XLV",
            places: &[(2, 45)],
            words: vec![variant("town")],
        },
        Need { why: "how it ends, Part II chapter LIII", places: &[(2, 53)], words: vec!["desnudo nací"] },
    ]
}

fn island_parts(wd: &Path) -> Vec<Part> {
    quote_parts(
        wd,
        r"(?im)^\W*island\W*\s*.*barataria",
        &island_needs(),
        &[(r"(?im)^\W*giver\W*\s*.*(duke|duque)", "the GIVER line must name the duke")],
    )
}

fn quixote_island() -> Task {
    book_task(
        "quixote-island",
        "a fact chain over four chapters hundreds of pages apart: the promise, the fallback, the gift and the end, \
         with the closing proverb repeated in three other chapters; the book is lightly edited, so the fallback \
         kingdom and the size of the town recalled from memory are wrong",
        "quijote.txt holds the Spanish text of Don Quixote (both parts, about 2 MB). Sancho Panza is promised an \
         island. Write ANSWER.md with: a line `ISLAND: <name of the island he finally governs>`, a line `GIVER: \
         <who gives it to him>`, and the evidence, one item per line, as `Part I, chapter VII: \"exact quote\"`: \
         (1) the chapter of Part One where his master first holds out an island as the reward for serving him, \
         (2) the chapter of Part One where, as Sancho grows impatient, his master names two kingdoms to fall back \
         on if no island turns up, (3) the chapter of Part Two where Sancho takes possession (the quote must also \
         give the size of the town, as this copy of the book states it), (4) the chapter where he gives it up, \
         with the words he says on leaving. Copy every quote exactly from this file; the chapter must be the one \
         that really holds it (an invented quote or a wrong part or chapter number spoils the answer).",
        |wd| Outcome::share(island_parts(wd)),
        island_solution,
    )
}

fn island_answer() -> String {
    format!(
        "ISLAND: Barataria\nGIVER: the Duke\n\n\
Part I, chapter VII: \"ganase, en quítame allá esas pajas, alguna ínsula, y le dejase a él por gobernador della\"\n\
Part I, chapter X: \"cuando faltare ínsula, ahí está {}, que te vendrán como anillo al dedo\"\n\
Part II, chapter XLV: \"llegó Sancho a un lugar de hasta {} de los mejores que el duque tenía ... que se llamaba la ínsula Barataria\"\n\
Part II, chapter LIII: \"desnudo nací, desnudo me hallo: ni pierdo ni gano\"\n",
        variant("kingdom"),
        variant("town")
    )
}

fn island_solution(wd: &Path) {
    write_answer(wd, &island_answer());
}

// ---------------------------------------------------------------- the thread of the island

/// Distinct chapters a full answer must cite, each with a verbatim quote about an ínsula.
///
/// Measured on the perturbed book (the test `the_thread_is_spread_over_the_whole_book` prints it):
/// 2129 kB, about 530k tokens, so 4.1 windows of 128k tokens. The word is in 42 chapters, 14 of
/// Part I and 28 of Part II; split into fifths of the book by size (a fifth is about one window)
/// they hold 7, 3, 11, 10 and 11. 24 is a little over half of the 42: too many for the opening
/// chapters or one dense stretch (the most any single fifth holds is 11), so the answer needs hits
/// from every fifth (the second has only 3 to choose from), at least 8 from Part I (14 exist) and
/// 10 from Part II (28 exist). Each quote must span at least `THREAD_QUOTE` normalized characters,
/// more than the one line a search prints, so every hit is read in context: about 24 reads of a
/// few lines each plus one search for the word with line numbers and one for the chapter headings,
/// well inside 40 steps, but far more text seen than fits one window if whole chapters are read.
const THREAD_MIN: usize = 24;
/// Chapters from each part, so neither part carries the answer alone.
const THREAD_PART_MIN: (usize, usize) = (8, 10);
/// Shortest quote that counts, in normalized characters.
const THREAD_QUOTE: usize = 90;
/// Quotes that exist only in the edited book: three inserted or changed sentences about the island.
const THREAD_EDITED_MIN: usize = 2;

/// The index of the fifth of the book (by size) a chapter starts in.
fn fifths(book: &BTreeMap<(u32, u32), String>) -> BTreeMap<(u32, u32), usize> {
    let total: usize = book.values().map(String::len).sum();
    let mut seen = 0;
    let mut out = BTreeMap::new();
    for (k, t) in book {
        out.insert(*k, (seen * 5 / total).min(4));
        seen += t.len();
    }
    out
}

fn thread_parts(wd: &Path) -> Vec<Part> {
    let (raw, orig) = match (book_text(), book_raw()) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return vec![(false, e)],
    };
    let book = chapters(&raw);
    let real = chapters(&orig);
    let answer = read(wd, "ANSWER.md");
    if answer.trim().is_empty() {
        return vec![(false, "ANSWER.md is missing or empty".into())];
    }
    let mut bad = Vec::new();
    let mut found: BTreeMap<(u32, u32), bool> = BTreeMap::new();
    for c in cites(&answer) {
        match valid(&c, &book) {
            Err(e) => bad.push(e),
            Ok((at, whole)) if !whole.contains("insula") => {
                bad.push(format!("{:?} (part {} chapter {}) is not about an ínsula", c.quote, at.0, at.1));
            }
            Ok((_, whole)) if whole.len() < THREAD_QUOTE => {
                bad.push(format!("{:?} is shorter than {THREAD_QUOTE} characters", c.quote));
            }
            Ok((at, _)) => {
                // edited: some fragment of the quote is not in the same chapter of the real book
                let edited = real.get(&at).is_none_or(|t| {
                    let hay = norm(t);
                    c.quote.replace('…', "...").split("...").map(norm).any(|f| f.len() >= 6 && !hay.contains(&f))
                });
                let e = found.entry(at).or_insert(false);
                *e = *e || edited;
            }
        }
    }
    let part = |p: u32| found.keys().filter(|k| k.0 == p).count();
    let fifth = fifths(&book);
    let covered: std::collections::BTreeSet<usize> = found.keys().map(|k| fifth[k]).collect();
    vec![
        (
            bad.is_empty() && !found.is_empty(),
            match bad.first() {
                Some(e) => format!("a citation is invented, mislabelled, too short or not about an ínsula: {e}"),
                None => "no citations found".to_string(),
            },
        ),
        (found.len() >= THREAD_MIN, format!("needs {THREAD_MIN} different chapters, found {}", found.len())),
        (
            part(1) >= THREAD_PART_MIN.0 && part(2) >= THREAD_PART_MIN.1,
            format!(
                "needs {} chapters of Part I and {} of Part II, found {} and {}",
                THREAD_PART_MIN.0,
                THREAD_PART_MIN.1,
                part(1),
                part(2)
            ),
        ),
        (
            covered.len() == 5,
            format!("the quotes must reach every fifth of the book, they reach {:?} of 0 to 4", covered),
        ),
        (
            found.values().filter(|e| **e).count() >= THREAD_EDITED_MIN,
            format!(
                "needs {THREAD_EDITED_MIN} quotes from the sentences about the island that only this copy of the \
                 book has, found {}",
                found.values().filter(|e| **e).count()
            ),
        ),
    ]
}

fn quixote_thread() -> Task {
    book_task(
        "quixote-insula-thread",
        "one thread spread over both parts and the whole 2 MB book (about four windows): every chapter where \
         an ínsula is mentioned, with part, chapter and an exact quote; a full answer needs hits from every fifth \
         of the book, kept while the window rolls over, and some sentences exist only in this edited copy",
        &format!(
            "quijote.txt holds the Spanish text of Don Quixote (both parts, about 2 MB). The word ínsula (or \
             ínsulas) comes up again and again, in Sancho's mouth and others', from the start of Part One to the \
             end of Part Two. Write ANSWER.md listing at least {THREAD_MIN} different chapters where it is \
             mentioned, at least {} from Part One and {} from Part Two, reaching every stretch of the book (not \
             just the first chapters or one dense stretch), one per line as `Part I, chapter VII: \"exact \
             quote\"`. Each quote must contain the word, copy at least {THREAD_QUOTE} characters (a full phrase, \
             longer than a single printed line) exactly from this file, and sit in the chapter you name. This \
             copy of the book has been edited, so a passage you remember may differ here or be missing: at least \
             {THREAD_EDITED_MIN} of your items must be sentences about an ínsula that you find only in this file. \
             An invented quote or a wrong part or chapter number spoils the answer.",
            THREAD_PART_MIN.0, THREAD_PART_MIN.1
        ),
        thread_outcome,
        thread_solution,
    )
}

fn thread_outcome(wd: &Path) -> Outcome {
    Outcome::share(thread_parts(wd))
}

/// The reference answer for a book: in every chapter that mentions an ínsula, a window of its first
/// mention at least `THREAD_QUOTE` characters long and free of quotation marks.
fn thread_answer(book: &BTreeMap<(u32, u32), String>) -> String {
    let mut picks: Vec<((u32, u32), String)> = Vec::new();
    for (at, text) in book {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        // a window of the flat text around the first mention that is long enough and clean of quotes
        let Some(i) = flat.to_lowercase().find("ínsula") else { continue };
        let mut start = i.saturating_sub(60);
        while !flat.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (i + 80).min(flat.len());
        while !flat.is_char_boundary(end) {
            end += 1;
        }
        let q = &flat[start..end];
        let q = q.split_once(' ').map_or(q, |x| x.1);
        let q = q.rsplit_once(' ').map_or(q, |x| x.0);
        if !q.contains(['"', '“', '”', '«', '»']) && norm(q).len() >= THREAD_QUOTE + 10 {
            picks.push((*at, q.to_string()));
        }
    }
    let mut lines = String::new();
    for ((p, c), q) in picks {
        lines.push_str(&format!("Part {}, chapter {}: \"{q}\"\n", if p == 1 { "I" } else { "II" }, c));
    }
    lines
}

fn thread_solution(wd: &Path) {
    write_answer(wd, &thread_answer(&chapters(&book_text().expect("the cached book"))));
}

// ---------------------------------------------------------------- share request

const APP: [(&str, &str); 10] = [
    ("api/app.py", include_str!("long2/notebox/api/app.py")),
    ("api/legacy.py", include_str!("long2/notebox/api/legacy.py")),
    ("api/middleware.py", include_str!("long2/notebox/api/middleware.py")),
    ("api/sessions.py", include_str!("long2/notebox/api/sessions.py")),
    ("api/settings.py", include_str!("long2/notebox/api/settings.py")),
    ("api/shares.py", include_str!("long2/notebox/api/shares.py")),
    ("web/config.php", include_str!("long2/notebox/web/config.php")),
    ("web/index.php", include_str!("long2/notebox/web/index.php")),
    ("web/lib/session.php", include_str!("long2/notebox/web/lib/session.php")),
    ("web/static/share.js", include_str!("long2/notebox/web/static/share.js")),
];

fn share_request() -> Task {
    Task {
        id: "share-request",
        trap: "document the exact request a page's own script sends for one action: the path, header names, cookie \
               name and body keys are spread over a PHP page, a script, a Flask blueprint, middleware and \
               settings, with a deprecated v1 route and an unregistered handler as lookalikes",
        group: "long2",
        files: APP.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect(),
        turns: vec![(
            1,
            "This folder holds Notebox, a small notes app: a PHP page and script in web/ and a Flask API in api/. \
             For the API documentation, write docs/share-request.http: the example HTTP request that the app's own \
             front end sends when a user shares note 42 with someone, exactly as the code builds it: request line, \
             the query, every header the code sets or requires for that call (including the session cookie and the \
             anti-forgery header, with placeholder values), a blank line and the body. Work only from the code in \
             this folder."
                .into(),
        )],
        check: |wd| Outcome::share(request_parts(wd)),
        solution: |wd| {
            let _ = std::fs::create_dir_all(wd.join("docs"));
            std::fs::write(wd.join("docs/share-request.http"), REQUEST).expect("a writable working directory");
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

const REQUEST: &str = "POST /api/v2/notes/42/shares?notify=1 HTTP/1.1\r\n\
Host: notes.example.test\r\n\
Cookie: nbsid=SESSION_ID\r\n\
X-NB-CSRF: CSRF_TOKEN\r\n\
X-NB-Client: web\r\n\
Content-Type: application/json\r\n\
\r\n\
{\"recipient_email\": \"sam@example.test\", \"permission\": \"viewer\", \"expires_days\": 7}\r\n";

fn request_parts(wd: &Path) -> Vec<Part> {
    let raw = read(wd, "docs/share-request.http");
    let text: String =
        raw.replace("\r\n", "\n").lines().filter(|l| !l.trim_start().starts_with("```")).collect::<Vec<_>>().join("\n");
    let text = text.trim_start();
    if text.is_empty() {
        return vec![(false, "docs/share-request.http is missing or empty".into())];
    }
    let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let mut it = first.split_whitespace();
    let (method, target) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    let headers: BTreeMap<String, String> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path_ok = Regex::new(r"^/api/v2/notes/\d+/shares$").expect("valid").is_match(path);
    let pairs: Vec<(&str, &str)> =
        query.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').unwrap_or((p, ""))).collect();
    let cookie = headers.get("cookie").cloned().unwrap_or_default();
    let cookie_names: Vec<&str> = cookie.split(';').filter_map(|c| c.split_once('=')).map(|(k, _)| k.trim()).collect();
    let json: Option<serde_json::Value> = serde_json::from_str(body.trim()).ok();
    let body_ok = json.as_ref().is_some_and(|v| {
        let Some(o) = v.as_object() else { return false };
        let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
        keys.sort();
        keys == ["expires_days", "permission", "recipient_email"]
            && o["recipient_email"].as_str().is_some_and(|e| e.contains('@'))
            && matches!(o["permission"].as_str(), Some("viewer" | "editor"))
            && o["expires_days"].as_i64().is_some_and(|d| (1..=30).contains(&d))
    });
    let ct = headers.get("content-type").map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase());
    vec![
        (method == "POST", format!("the method is {method:?}, the code sends POST")),
        (path_ok, format!("the path {path:?} is not the v2 share route with a note id")),
        (
            pairs.len() == 1 && pairs[0].0 == "notify" && matches!(pairs[0].1, "0" | "1"),
            format!("the query {query:?} should be exactly notify=0 or notify=1"),
        ),
        (headers.get("host").is_some_and(|h| h == "notes.example.test"), "Host should be the configured base host".into()),
        (cookie_names == ["nbsid"] && !cookie.ends_with('='), format!("the cookie should be only the session cookie nbsid, got {cookie:?}")),
        (
            headers.get("x-nb-csrf").is_some_and(|v| !v.is_empty())
                && !headers.contains_key("x-csrf-token")
                && !body.contains("csrf"),
            "the anti-forgery token goes in the X-NB-CSRF header, nowhere else".into(),
        ),
        (headers.get("x-nb-client").is_some_and(|v| v == "web"), "the X-NB-Client: web header is missing".into()),
        (ct.as_deref() == Some("application/json"), "Content-Type should be application/json".into()),
        (body_ok, "the body should be JSON with recipient_email, permission (viewer or editor) and expires_days (1 to 30) only".into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(task: &Task) -> tempfile::TempDir {
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        wd
    }

    /// Tests use only an already cached book: no download from the test run.
    fn book_cached() -> bool {
        cache_file().is_some_and(|p| cached_ok(&p))
    }

    fn by_id(id: &str) -> Task {
        tasks().into_iter().find(|t| t.id == id).unwrap()
    }

    fn failing(parts: Vec<Part>) -> Vec<String> {
        parts.into_iter().filter(|(ok, _)| !ok).map(|(_, why)| why).collect()
    }

    #[test]
    fn roman_numerals_and_labels_parse() {
        assert_eq!(roman("XLV"), Some(45));
        assert_eq!(roman("LIII"), Some(53));
        assert_eq!(roman("primero"), Some(1));
        assert_eq!(roman("12"), Some(12));
        let c = cites(
            "- Part II, Capítulo X: «como se pareció cuando dijo»\nII.3: \"la aventura de los molinos\"\nno quote",
        );
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].part, c[0].chapter), (Some(2), Some(10)));
        assert_eq!((c[1].part, c[1].chapter), (Some(2), Some(3)));
        assert_eq!(norm("Frestón, ¡el SABIO!"), "freston el sabio");
    }

    #[test]
    fn the_book_splits_into_two_parts_of_52_and_74_chapters() {
        if !book_cached() {
            return;
        }
        let b = chapters(&book_raw().unwrap());
        assert_eq!(b.keys().filter(|k| k.0 == 1).count(), 52);
        assert_eq!(b.keys().filter(|k| k.0 == 2).count(), 74);
        assert!(norm(&b[&(1, 8)]).contains("molinos de viento"));
        // the proverb is in four chapters, only one is the end of the government
        let n = norm("desnudo nací, desnudo me hallo");
        let hits: Vec<_> = b.iter().filter(|(_, t)| norm(t).contains(&n)).map(|(k, _)| *k).collect();
        assert!(hits.contains(&(2, 53)) && hits.len() >= 3, "{hits:?}");
    }

    const BOOK_TASKS: [&str; 3] = ["quixote-windmills", "quixote-island", "quixote-insula-thread"];

    /// What someone who knows the novel by heart, and has not read this copy, would write.
    const MEMORY_WINDMILLS: &str = "VERDICT: yes\n\n\
Part I, chapter VIII: \"descubrieron treinta o cuarenta molinos de viento que hay en aquel campo\"\n\
Part I, chapter VIII: \"aquel sabio Frestón que me robó el aposento y los libros ha vuelto estos gigantes en molinos\"\n\
Part I, chapter I: \"del poco dormir y del mucho leer, se le secó el celebro, de manera que vino a perder el juicio\"\n\
Part II, chapter X: \"como se pareció cuando dijo que los molinos de viento eran gigantes\"\n";

    #[test]
    fn closed_variants_have_no_book_and_the_same_question() {
        for id in BOOK_TASKS {
            let open = by_id(id);
            let closed = by_id(&format!("{id}-closed"));
            assert_eq!(closed.group, "closed");
            assert!(closed.setup.is_none() && closed.files.is_empty());
            let wd = fresh(&closed);
            assert_eq!(std::fs::read_dir(wd.path()).unwrap().count(), 0, "{id}: closed folder is not empty");
            assert!(!wd.path().join("quijote.txt").exists());
            assert!(closed.turns[0].1.contains("NOT in this folder"));
            assert!(closed.turns[0].1.ends_with(&open.turns[0].1));
            let wd = fresh(&open);
            assert!(wd.path().join("quijote.txt").exists(), "{id}: the open task has its book");
        }
    }

    #[test]
    fn closed_variants_grade_like_the_open_task_and_memory_does_not_pass() {
        if !book_cached() {
            return;
        }
        for id in BOOK_TASKS {
            let t = by_id(&format!("{id}-closed"));
            let wd = fresh(&t);
            assert!(!(t.check)(wd.path()).ok, "{id}-closed: an empty answer passes");
            (t.solution)(wd.path());
            let got = (t.check)(wd.path());
            assert!(got.ok && got.score == 1.0, "{id}-closed: {}", got.detail);
        }
        // the answer from memory of the real book fails where the edits are
        let t = by_id("quixote-windmills-closed");
        let wd = fresh(&t);
        write_answer(wd.path(), MEMORY_WINDMILLS);
        let f = failing(windmill_parts(wd.path()));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        assert!(!(t.check)(wd.path()).ok);
        // the same, with memory of the island chain
        let t = by_id("quixote-island-closed");
        let wd = fresh(&t);
        let memory = island_answer().replace(variant("kingdom"), "el reino de Dinamarca o el de Soliadisa");
        write_answer(wd.path(), &memory);
        assert!(!(t.check)(wd.path()).ok);
        let memory = island_answer().replace(variant("town"), "mil vecinos, que era");
        write_answer(wd.path(), &memory);
        assert!(!(t.check)(wd.path()).ok);
        // and for the thread: the real book's sentences, not the edited copy's
        let real = thread_answer(&chapters(&book_raw().unwrap()));
        write_answer(wd.path(), &real);
        let f = failing(thread_parts(wd.path()));
        assert!(!f.is_empty(), "the real book's thread passes");
    }

    #[test]
    fn the_perturbation_is_deterministic_and_minimal() {
        if !book_cached() {
            return;
        }
        let raw = book_raw().unwrap();
        let a = perturb(&raw).unwrap();
        assert_eq!(a, perturb(&raw).unwrap(), "the perturbation is not repeatable");
        // the same edits drawn again give the same variants
        for e in &EDITS {
            assert_eq!(variant(e.id), variant(e.id));
            assert!(e.variants.contains(&variant(e.id)));
        }
        // minimal: same lines but one per edit, every other line byte-identical
        let (ra, rb): (Vec<&str>, Vec<&str>) = (raw.lines().collect(), a.lines().collect());
        assert_eq!(ra.len(), rb.len());
        let changed: Vec<usize> = (0..ra.len()).filter(|&i| ra[i] != rb[i]).collect();
        assert_eq!(changed.len(), EDITS.len(), "{changed:?}");
        let grown = a.len() as i64 - raw.len() as i64;
        assert!(0 < grown && grown < 1_500, "{grown}");
        // every edit shows in the book and the real wording is gone from the swaps
        for e in &EDITS {
            assert!(a.contains(variant(e.id)));
            if !e.insert {
                assert!(!a.contains(e.find), "{} is still in the book", e.find);
            }
        }
        // a book where an anchor is missing is refused
        assert!(perturb("nothing to find").is_err());
    }

    #[test]
    fn the_thread_is_spread_over_the_whole_book() {
        if !book_cached() {
            return;
        }
        let book = chapters(&book_text().unwrap());
        let fifth = fifths(&book);
        let with: Vec<(u32, u32)> = book.iter().filter(|(_, t)| norm(t).contains("insula")).map(|(k, _)| *k).collect();
        let per_part = |p: u32| with.iter().filter(|k| k.0 == p).count();
        let mut per_fifth = [0usize; 5];
        for k in &with {
            per_fifth[fifth[k]] += 1;
        }
        let kb: usize = book.values().map(String::len).sum::<usize>() / 1000;
        eprintln!(
            "book {kb} kB; chapters with an ínsula: {} (Part I {}, Part II {}); per fifth {per_fifth:?}",
            with.len(),
            per_part(1),
            per_part(2)
        );
        assert!(per_fifth.iter().all(|&n| n > 0), "{per_fifth:?}");
        assert!(with.len() >= THREAD_MIN + 8, "enough chapters to choose from: {}", with.len());
        assert!(per_part(1) >= THREAD_PART_MIN.0 + 4 && per_part(2) >= THREAD_PART_MIN.1 + 4);
        // the edited sentences are in chapters that had no ínsula before
        let real = chapters(&book_raw().unwrap());
        for id in ["count", "boat", "wood"] {
            let at = book.iter().find(|(_, t)| norm(t).contains(&norm(variant(id)))).map(|(k, _)| *k).unwrap();
            assert!(!norm(&real[&at]).contains("insula"), "{id}: chapter {at:?} already had an ínsula");
        }
    }

    #[test]
    fn book_tasks_build_pass_their_reference_and_fail_otherwise() {
        if !book_cached() {
            return;
        }
        for id in BOOK_TASKS {
            let t = by_id(id);
            let wd = fresh(&t);
            assert!(std::fs::metadata(wd.path().join("quijote.txt")).unwrap().len() > 1_500_000);
            assert!(!(t.check)(wd.path()).ok, "{id}: the untouched task passes");
            (t.solution)(wd.path());
            let got = (t.check)(wd.path());
            assert!(got.ok && got.score == 1.0, "{id}: {}", got.detail);
        }
    }

    #[test]
    fn windmill_decoys_fail() {
        if !book_cached() {
            return;
        }
        let t = by_id("quixote-windmills");
        let wd = fresh(&t);
        let run = |text: &str| {
            write_answer(wd.path(), text);
            failing(windmill_parts(wd.path()))
        };
        let good = windmill_answer();
        // the book was edited: the count from memory is not in this copy
        let f = run(MEMORY_WINDMILLS);
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        // the other chapter VIII (Part Two) is not where the windmills are
        let f = run(&good.replace("Part I, chapter VIII: \"aquel", "Part II, chapter VIII: \"aquel"));
        assert!(f.iter().any(|w| w.contains("mislabelled")), "{f:?}");
        // an invented quote
        let f = run(&good.replace("que hay en aquel campo", "que había en aquel campo"));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        // right quote, right book, wrong chapter for the memory of Part Two
        let f = run(&good.replace("Part II, chapter X:", "Part II, chapter XI:"));
        assert!(!f.is_empty());
        // the wrong verdict
        let f = run(&good.replace("VERDICT: yes", "VERDICT: no"));
        assert_eq!(f.len(), 1, "{f:?}");
        // the windmill sentence without the count is not the evidence asked for
        let f =
            run(&good.replace(&format!("descubrieron {} que hay", variant("windmills")), "descubrieron unos que hay"));
        assert!(!f.is_empty());
        // an ellipsis is tolerated, accents and case are not held against a quote
        let f = run(&good.replace(
            "aquel sabio Frestón que me robó el aposento y los libros ha vuelto",
            "AQUEL SABIO FRESTON que me robo ... ha vuelto",
        ));
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn island_decoys_fail() {
        if !book_cached() {
            return;
        }
        let t = by_id("quixote-island");
        let wd = fresh(&t);
        let good = island_answer();
        let run = |text: &str| {
            write_answer(wd.path(), text);
            failing(island_parts(wd.path()))
        };
        // the same proverb, real but from the chapter where Sancho first says it (Part II chapter VIII)
        let f = run(&good.replace("Part II, chapter LIII", "Part II, chapter VIII"));
        assert!(f.len() == 1 && f[0].contains("how it ends"), "{f:?}");
        let f = run(&good.replace("Barataria\n", "Baratario\n"));
        assert!(!f.is_empty());
        let f = run(&good.replace("GIVER: the Duke", "GIVER: the Priest"));
        assert!(!f.is_empty());
        let f = run(&good.replace("Part I, chapter X:", "Part I, chapter XI:"));
        assert!(f.iter().any(|w| w.contains("mislabelled")), "{f:?}");
        // memory: the real kingdom and the real size of the town are not in this copy
        let f = run(&good.replace(variant("kingdom"), "el reino de Dinamarca o el de Soliadisa"));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        let f = run(&good.replace(variant("town"), "mil vecinos, que era"));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    }

    #[test]
    fn thread_decoys_fail() {
        if !book_cached() {
            return;
        }
        let t = by_id("quixote-insula-thread");
        let wd = fresh(&t);
        (t.solution)(wd.path());
        let good = read(wd.path(), "ANSWER.md");
        let lines: Vec<&str> = good.lines().collect();
        assert!(lines.len() >= THREAD_MIN + 8, "{}", lines.len());
        let run = |text: &str| {
            write_answer(wd.path(), text);
            failing(thread_parts(wd.path()))
        };
        assert!(run(&good).is_empty());
        // too few chapters: only the first half of the list
        let f = run(&lines[..lines.len() / 2].join("\n"));
        assert!(!f.is_empty());
        // all of Part I only: not spread over the book
        let part1: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("Part I,")).collect();
        let f = run(&part1.join("\n"));
        assert!(f.iter().any(|w| w.contains("every fifth") || w.contains("Part II")), "{f:?}");
        // one line mislabelled or invented spoils the whole answer
        let f = run(&good.replacen("Part I, chapter", "Part II, chapter", 1));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        let f =
            run(&format!("{good}Part II, chapter X: \"una ínsula que Cervantes jamás escribió en este capítulo\"\n"));
        assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
        // short quotes and quotes about something else do not count
        let f = run(&format!("{good}Part I, chapter VII: \"alguna ínsula, y le dejase a él por gobernador\"\n"));
        assert!(f.iter().any(|w| w.contains("shorter")), "{f:?}");
        let f = run(&format!("{good}Part I, chapter VIII: \"aquel sabio Frestón que me robó el aposento y los libros ha vuelto estos gigantes en molinos\"\n"));
        assert!(f.iter().any(|w| w.contains("not about an")), "{f:?}");
        // the same chapter ten times is one chapter
        let f = run(&vec![lines[0]; 40].join("\n"));
        assert!(f.iter().any(|w| w.contains("different chapters")), "{f:?}");
        // without the edited sentences the answer is incomplete
        let real = chapters(&book_raw().unwrap());
        let keep: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                let c = &cites(l)[0];
                norm(&real[&(c.part.unwrap(), c.chapter.unwrap())]).contains(&norm(&c.quote))
            })
            .collect();
        assert!(keep.len() < lines.len());
        let f = run(&keep.join("\n"));
        assert!(f.iter().any(|w| w.contains("only this copy")), "{f:?}");
    }

    #[test]
    fn the_app_is_modest_and_the_share_request_reference_passes() {
        let t = by_id("share-request");
        let lines: usize = t.files.iter().map(|(_, s)| s.lines().count()).sum();
        assert!((150..=300).contains(&lines), "{lines}");
        let wd = fresh(&t);
        assert!(!(t.check)(wd.path()).ok);
        (t.solution)(wd.path());
        let got = (t.check)(wd.path());
        assert!(got.ok && got.score == 1.0, "{}", got.detail);
    }

    #[test]
    fn share_request_decoys_fail() {
        let t = by_id("share-request");
        let wd = fresh(&t);
        std::fs::create_dir_all(wd.path().join("docs")).unwrap();
        let run = |text: &str| {
            std::fs::write(wd.path().join("docs/share-request.http"), text).unwrap();
            failing(request_parts(wd.path()))
        };
        let lf = REQUEST.replace("\r\n", "\n");
        assert!(run(&lf).is_empty());
        assert!(run(&format!("```http\n{lf}```\n")).is_empty());
        // the deprecated v1 route: other path, PHPSESSID, _csrf, form body
        let v1 = "POST /api/v1/notes/42/share HTTP/1.1\nHost: notes.example.test\nCookie: PHPSESSID=x\nContent-Type: application/x-www-form-urlencoded\n\n_csrf=t&to=sam%40example.test&level=read\n";
        assert!(run(v1).len() >= 6);
        // one wrong fact each
        assert_eq!(run(&lf.replace("/api/v2/", "/api/v1/")).len(), 1);
        assert_eq!(run(&lf.replace("nbsid", "nb_session")).len(), 1);
        assert_eq!(run(&lf.replace("X-NB-CSRF", "X-CSRF-Token")).len(), 1);
        assert_eq!(run(&lf.replace("?notify=1", "")).len(), 1);
        assert_eq!(run(&lf.replace("X-NB-Client: web\n", "")).len(), 1);
        assert_eq!(run(&lf.replace("application/json", "application/x-www-form-urlencoded")).len(), 1);
        assert_eq!(run(&lf.replace("\"permission\"", "\"level\"")).len(), 1);
        assert_eq!(run(&lf.replace("viewer", "read")).len(), 1);
        assert_eq!(run(&lf.replace("\"expires_days\": 7", "\"expires_days\": 90")).len(), 1);
        assert_eq!(run(&lf.replace("POST", "PUT")).len(), 1);
    }
}
