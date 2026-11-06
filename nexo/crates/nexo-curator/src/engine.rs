//! The goal engine: what of the conversation the next model call sees.

use std::collections::BinaryHeap;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::{json, Map, Value};

use crate::goal::{action, head_slice, Threads};
use crate::hindsight::{Interner, Set, Sym, Tracer, UKind};
use crate::json::{est, est_str, py_eq, str_chars, strip_cc};
use crate::learn::{Featurizer, MODEL};
use crate::ledger::{
    fold_forms, lines_of, prior, said_lines, select, Names, ERROR_MARKS, ON_DISK, PER_MESSAGE, POOL, SAID,
};
use crate::mechanical::{squeeze, MARK};
use crate::msg::{blocks, get_str, key_of, py_str, text_block, text_of};
use crate::py::{self, bisect_left, Re};
use crate::rules::{clip, rules_in, Rules};
use crate::telegraph::compress;
use crate::tools::{
    classify, edit_size, full_write, heredoc_writes, partial_read, printed_files, prints_files, read_range,
    shell_writes, Kind,
};

static TEST_COMMAND: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"\b(?:pytest|py\.test|unittest|tox|nox|jest|vitest|mocha|rspec|ctest|phpunit|nextest)\b|\b(?:cargo|go|npm|pnpm|yarn|bun|deno|dotnet|mix|gradlew?|mvnw?|swift|zig|sbt|lein|bazel|stack|cabal|flutter|dart|rails|rake|artisan|playwright|manage\.py)\s+(?:run\s+)?(?:test|spec)\b|\bnode\s+--test\b|\bmake\s+(?:test|check)\b",
        0,
    )
});
/// A failing test run, in the output shapes the runners of most ecosystems share: a line that starts with a
/// failure word (`--- FAIL:`, `FAIL`, `FAILED`, `FAILURES!`, `Failed!`, `BUILD FAILURE`, `not ok 2`, `✕`, `✖`,
/// `# fail 1`, `ℹ fail 1`, `make: *** ... Error 2`), a count of one or more failures (`1 failed`, `2 failing`,
/// `1 failure`, `1 tests failed`) or a labelled one where a summary prints it (`Failures: 1,`, `Failed: 1`,
/// `errors=1)`, `, Failed 1,`), or a non-zero exit status at a line start (`exit status 1`, `[exit code 1]`) or
/// after `failed with` on a line that is not a passing test's (`✓`, `ok `, `PASS`). A word and a number in a
/// test name, a package path or a log line (`returns error 404`, `api/errors 1.2s`) are not one. A run piped
/// through `tail` exits 0 whatever its tests did.
static TEST_FAILED: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*(?:--- FAIL:|FAIL\b|FAILED\b|FAILURES?!|Failed!|(?:\[\w+\]\s+)?BUILD FAIL|not ok \d|[✕✗✖]\s|[#ℹ]\s*fail\s+[1-9]|make(?:\[\d+\])?: \*\*\* .*\bError [1-9])|\b[1-9]\d* (?:tests? )?(?:failed|failing|failures?|errors?)\b(?![ \t]*[:=]?[ \t]*\d)|(?i:(?:^|[,;(\-]|\btests?)[ \t]*(?:failures?|failed|errors?)[ \t]*[:=][ \t]*[1-9]\d*(?=[ \t]*(?:[,.;)]|$))|[,;(][ \t]*(?:failures?|failed|errors?)[ \t]+[1-9]\d*(?=[ \t]*[,;)]))|(?i:^\s*\[?exit (?:status|code)[ \t]*:?[ \t]*[1-9]\d*\b|^(?![ \t]*(?-i:[✓✔√]|ok\s|PASS\b))[^\n]*?\b(?:failed|finished|exited) with (?:exit )?(?:code|status) [1-9]\d*\b)",
        py::M,
    )
});
static FAILURE_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r#"FAIL|ERROR|Error|Traceback|File "|panicked|fail|assert|✗|✕|●|^\s*not ok\b|\.\w+:\d+"#, py::M)
});
/// The name of a failing test, in the shapes runners print it: after `::`, `FAIL:`, `--- FAIL:` or `, in`
/// (`test_x`, `TestX`), `test a::b ... FAILED` (its last part), `Class > name() FAILED`, a `●`/`✕` line,
/// `rspec <file>:<line>` and `not ok N - name`. Each alternative has one group; `failed_tests` takes the one
/// that took part.
static FAILED_TEST: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^\s*test\s+(?:[\w$]+::)*([\w$]+)\s+\.\.\.\s+FAILED|^\s*[\w.$]+\s+>\s+([\w$]+)(?:\(\))?\s+FAILED|^\s*[●✕✗]\s+(?!Console\b|Test suite failed)(.+?)(?:\s+\(\d+(?:\.\d+)?\s*m?s\))?\s*$|^\s*rspec\s+(\S+:\d+)|^\s*not ok \d+ - (.+?)\s*$|(?:::|\b(?:FAIL|FAILED|ERROR):?\s+|---\s+FAIL:\s+|,\s+in\s+)(test_\w+|Test[A-Z_]\w*)",
        py::M,
    )
});
static FAILURE_COUNT: LazyLock<Re> = LazyLock::new(|| {
    Re::new(r"\b(?:failures|errors)=(\d+)|\b(\d+) (?:tests? )?(?:failed|failing|failures?|errors?)\b", 0)
});

/// The failing tests a run's failure lines name (`FAILED_TEST`).
fn failed_tests(f: &str) -> Vec<&str> {
    FAILED_TEST
        .captures_all(f)
        .iter()
        .filter_map(|c| c.iter().skip(1).flatten().next().map(|&(a, b)| &f[a..b]))
        .collect()
}
static TEST_NAME: LazyLock<Re> = LazyLock::new(|| Re::new(r"(?:^|[/_.])(?:tests?|spec)(?=[/_.]|$)", 0));
// A line the agent marked as a finding, as its plan or as a rule it was given, in its own text; as a
// list item or bold too.
static NOTE_LINE: LazyLock<Re> = LazyLock::new(|| {
    Re::new(
        r"^[ \t]*(?:[-*•][ \t]*|\d+[.)][ \t]*)?(?:\*\*)?(Finding|Plan|Rule)(?:\*\*)?:(?:\*\*)?[ \t]*(.+?)[ \t]*$",
        py::M,
    )
});
const NOTE_CHARS: usize = 400;
/// The name of the tool whose input is a note (`Finding:`/`Plan:`/`Rule:` lines kept by the curator).
pub const NOTE_TOOL: &str = "note";
/// The tool the host may offer to bring a message back exactly, by the `m#####` name the curator gives it.
pub const RECALL_TOOL: &str = "recall";
/// The arguments of an edit that hold the text it replaced, in the shapes editor tools name them.
const REPLACED_ARGS: &[&str] = &["old_string", "old_str", "old_text"];
/// Output messages a closed request names by their `m#####` number before the rest are counted.
const RECALL_NAMED: usize = 6;
/// The latest calls for the same file or command `asked_back` looks through for the copy a call asks for
/// again, and the latest edits it looks through for one that changed the file since: a copy further back
/// is let be, and one with more edits after it is taken as changed. Both keep a request's work bounded
/// however long the session.
const ASKED_CALLS: usize = 32;
const ASKED_EDITS: usize = 256;

/// The text of a block that may carry note lines: a text block, or the input of a `note` tool call.
fn note_text(b: &Value) -> Option<String> {
    match b.get("type").and_then(Value::as_str) {
        Some("text") => Some(get_str(b, "text").to_string()),
        Some("tool_use") if get_str(b, "name") == NOTE_TOOL => Some(crate::msg::flat(b.get("input"))),
        _ => None,
    }
}
/// What a silent agent is asked for (`Params.nudge`), at the end of the last tool result: a `note` call,
/// since a model that only calls tools answers a tool, not a request for text.
pub const NUDGE: &str = "[curator] Before the next step, keep what these results settled with the `note` tool: one line beginning with \"Finding:\" for each fact (values, formulas, formats, causes) and one line beginning with \"Plan:\" with what is left; then go on with what you were doing (a note records what you read, it does not stand in for reading the rest). The older results will leave this context; the notes stay.";
/// The same request for a host that offers no `note` tool: the lines go at the start of the reply, where
/// the curator reads them as well.
pub const NUDGE_SAID: &str = "[curator] Before the next step, keep what these results settled: start your next reply with one line beginning with \"Finding:\" for each fact (values, formulas, formats, causes) and one line beginning with \"Plan:\" with what is left; then go on with what you were doing (a note records what you read, it does not stand in for reading the rest). The older results will leave this context; the lines stay.";
/// Tokens of one file the agent wrote kept in the anchor; a longer one keeps its head and tail.
const WROTE_TOKENS: i64 = 1500;
/// Tool outputs shorter than this (in tokens) are never left out in place: the stub that would stand for
/// them is a third of their size or more.
const SMALL_RESULT: i64 = 100;
/// The distinct calls a closed request names on its line.
const CLOSED_SHOWN: usize = 8;
const OUTCOME_CHARS: usize = 700;
/// The most characters of the person's request that `recite` sends again.
const RECITE_CHARS: usize = 2000;
/// `Params.stall`: the share of new lines under which the last outputs count as going round.
const STALL_NEW: f64 = 0.1;

/// The settings of an engine (see the spec in `lib.rs`).
#[derive(Clone, Debug)]
pub struct Params {
    pub threshold: f64,
    pub protect: i64,
    pub check: i64,
    pub ratio: f64,
    pub margin: f64,
    pub floor: f64,
    pub every: i64,
    pub least: f64,
    pub cap: i64,
    pub turn_head: bool,
    pub eoq: i64,
    pub ledger: i64,
    pub link: f64,
    pub cards: i64,
    pub birth: Vec<String>,
    /// Tokens of the agent's findings and plan kept in the anchor (0: off).
    pub notes: i64,
    /// Tokens the person's standing instructions take before the findings and the pinned files give
    /// way to them (0: no rules block). A rule is never dropped for its size: past this budget the other
    /// blocks shrink instead.
    pub rules: i64,
    /// Tokens of the files the person named, as last read, and of the files the agent wrote, as last
    /// written, kept in the anchor (0: off).
    pub pin: i64,
    /// Agent steps without a `Finding:`, `Plan:` or `Rule:` line after which the last tool result asks
    /// for them, at most once per as many steps (0: never).
    pub nudge: i64,
    /// The host adds the curator's directives to the system prompt (0: it does not).
    pub directives: i64,
    /// A tool result of at least this many tokens asks for notes on the next step, and a result read before
    /// a later note leaves the view once the next request begins (0: off).
    pub settle: i64,
    /// A run of at least this many lines of a tool output that an earlier output holds too, and that
    /// earlier output is whole in the view, goes as one line that points to it; the run comes back whole
    /// once that output leaves the view or is cut (0: off). With `asked`, a range the agent asks for again goes
    /// whole: it means to look at it closely, and a pointer in its place sends it looking for a way round.
    pub dedup: i64,
    /// Past the cap, results go by need before the oldest messages leave: first the parts a later
    /// output in the view holds line for line, then the parts least likely to be needed (false: the
    /// oldest messages leave as soon as what the model rules out is not enough).
    pub cut_need: bool,
    /// Past the cap, only the parts a later output in the view holds line for line go; the cut that loses
    /// material waits until the view is past twice the cap, and below that the usual rent rule runs.
    pub cut_copies: bool,
    /// Past the cap, the parts of results that tell the most per token stay and the rest go (`Engine::cover`)
    /// before the oldest messages leave.
    pub cut_cover: bool,
    /// A request the agent closed with a reply (its last message holds text and no call) keeps, once the
    /// next request begins, only that request and that reply: the calls made for it and their outputs leave
    /// the view, and one line on the request says which (0: off). The reply is what the agent took from
    /// them, the way a note is.
    pub close: i64,
    /// How much of the calls so far `rent_close` counts on as still to come (0: only what keeping has cost).
    pub ahead: f64,
    /// Below the cap too, the parts of results whose every line a later output in the view sends are put to
    /// the rent rule with the parts the model rules out: whoever reads the view loses nothing by them, and
    /// they go once keeping them has cost more than the cache they break (0: only past the cap, `cut`).
    pub sup: i64,
    /// With `close`, the steps of an answered request leave only once the latest request is on another
    /// topic (thread): while the requests go on with the same topic, what was read for one is what the next
    /// works from, and a reply holds little of it (0: they leave as soon as the request is answered).
    pub topic: i64,
    /// Tokens the other topics must hold together before they leave the view: leaving one out costs a
    /// step to bring it back when it was needed, and saves in proportion to its size (0: they always leave).
    pub apart: i64,
    /// With `close`, a step of an answered request leaves once a `Finding:` or `Plan:` line the agent wrote
    /// after it names its source (the part of a file name that sets it apart from the other files read),
    /// and not before, whatever the topic: what was read leaves when its facts are noted (0: off). The
    /// output of a call whose files later notes all name is not kept for a request that names its file,
    /// once a later request on the file got its reply with no new read of it: the note proved it stands
    /// in for the read. An agent that writes no notes keeps the steps `close_unused` lets go. Off by
    /// default: in the golden sessions the proof comes when leaving already rewrites much of the cached
    /// view, and leaving before the proof made the agent go back to a file its note did not cover.
    pub noted: i64,
    /// With `close`, a step that read a file the latest request names stays, whatever else would let it go:
    /// the request is about that file (0: off).
    pub named: i64,
    /// With `named`, a step that read a file the thread's first message names stays until a later request
    /// of the thread names that file, once a later request took up another file of that list: the list is
    /// being worked through, and no request has yet been about this one (0: off).
    pub mission: i64,
    /// With `close`, a step brought back leaves again only once its rent since it went idle pays for
    /// bringing it back once more: it was needed after it left, so the guess of the calls to come does not
    /// count for it (0: off). A step the agent brought back by itself counts as brought back too: a
    /// `recall` of its output while the view left it out, or the same read or command made again then
    /// (`asked_back`).
    pub sticky: i64,
    /// With `close`, the outputs of the request still open go idle when no later agent message draws on
    /// them (`live_drop`), and leave when that pays (0: off).
    pub live: i64,
    /// With `live`, an output that leaves keeps its leads: the lines with a name no other output holds,
    /// of a form the agent has written before (`loc_3D60` after `loc_2A10`, `user_42` after `user_7`): a
    /// reference not yet followed (0: off).
    pub lead: i64,
    /// Recall brings back, for a word, the steps whose calls went for it (`cat auth.rs` for `auth`) over
    /// those that only came across it (an `ls` that printed `auth.rs`), and takes those words first (0: off).
    pub aim: i64,
    /// With `live`, a name the agent's call went for keeps an older output in use only if the call's own
    /// output lacks it: once `grep loc_1EF8` has printed what `loc_1EF8` leads to, the listing that
    /// pointed there is not what the agent works from (0: off).
    pub follow: i64,
    /// With `live`, an output goes idle only once the agent has written about what it read since: text,
    /// or a note. Calls alone are the agent still looking; what it read is not yet digested (0: off).
    pub digest: i64,
    /// The size cap (`eoq`) leaves out of its count the outputs the agent has not written about yet (no text and
    /// no note after them): material the agent is still reading is not what makes a context costly to carry,
    /// and taking it away leaves the agent blind to it (0: off).
    pub hold: i64,
    /// Below the cap and past it alike, the latest outputs the agent has not written about yet (no text and
    /// no note after them), up to this many tokens, leave only as an exact copy of a later output, never on a
    /// guess that they are not needed: the agent is still working from them, and a new request from the person
    /// does not change that. Older ones past the budget may go on a guess, so a session where the agent never
    /// writes still has a bounded view (0: a guess may take any).
    pub still: i64,
    /// A call the agent makes again just as it made it the step before, that returns the same output again,
    /// adds nothing: in a run of such steps the first and the latest stay, the ones between leave, and the
    /// latest carries a line that counts the run. A history of the same step over and over is what keeps a
    /// model repeating it, and it is sent again whole on every step (0: off, the default: the folded view
    /// stays the same from one step to the next, call ids and the count aside, and a model at a fixed
    /// temperature sent a view it was sent before takes the same step again; cycle 56).
    pub loops: i64,
    /// With `dedup`, an output that is mostly lines already in the view goes whole: the agent asked for it again,
    /// as it did not find or trust the copy above (0: such an output goes as pointers too; 2: only when most of its
    /// lines went as a pointer once already, so the first copy asked again still goes as one; 3: as 1, but an
    /// output an earlier call gave, line for line, goes as one line that says so, and that nothing changed when
    /// it was the same call).
    pub asked: i64,
    /// With `dedup`, the output that has just arrived goes whole, as it came: the agent asked for it to look at
    /// it now, and a pointer in its place sends it looking for a way round (it asks again, reads it by other
    /// means, or distrusts the tool). Its runs go as pointers from the next request on, which rewrites only that
    /// one message, at the end of what the cache holds (0: they go as pointers as the output arrives).
    pub fresh: i64,
    /// The note that takes the place of an output left out says only that it left and how to bring it back:
    /// in a long open request every such note is sent again on each step, so its words add up (0: off).
    pub brief: i64,
    /// A request still open after this many steps of the agent, then twice as many, four times…, has its own
    /// words sent again at the end of the latest output, once each time: after a long run of reads the request
    /// sits far above them, where a model attends to it least. The line is the person's request as written,
    /// nothing added; it is put in as that output arrives and never moves, so the cache keeps it (0: off).
    pub recite: i64,
    /// The person's words are a new request wherever they come, alone or with the results of a step (a
    /// host that adds the next request to them, a person who writes while the agent works); which topic it
    /// belongs to stays the links' call. A request left for a newer one is over (0: off).
    pub task: i64,
    /// A call made again says how often it ran in the session and whether its output changed: "ran 3
    /// times, the same output each time" tells the agent that reading it once more will not help, "changed"
    /// that the thing it reads moves. Said as the output arrives, so the cache keeps it (0: off).
    pub reruns: i64,
    /// Over the last `stall` outputs of the open request, the share of their lines never seen before in the
    /// session: under `STALL_NEW` the agent is going round what it has, and the request comes again in the
    /// person's words with that fact (step, new lines of the last outputs). Then after twice the steps, and so
    /// on. Takes the place of `recite`'s fixed count; needs `dedup` (it counts the lines) (0: off).
    pub stall: i64,
    /// A read of a file that a later edit or write of it (one that did not fail) has changed is out of date:
    /// past the cap it leaves with the parts a later output holds line for line, before any part that would
    /// leave on a guess, and the note left in its place names the file and the message of the edit. Decided
    /// only when the view is cut, so the view between two cuts stays as it was sent (0: off).
    pub stale: i64,
    /// The provider keeps more than the last request in its cache: the last view sent on each topic (a
    /// pocket) lives `life` seconds. A request back on a topic whose pocket lives reads it from the cache, so
    /// the switch costs nothing. How long the provider keeps a view is also learned from what it says it read
    /// from its cache (`Curator::cached`). 2: also, a view whose pocket is gone is read again anyway, so it is
    /// cut down then, and a cut past where the cache stops is priced as free: cheaper after long pauses, but
    /// what is cut early is missed more (0: off, the cache holds the last view).
    pub pockets: i64,
    /// Seconds a pocket is taken to live at most (`pockets`); 0: only the last view sent, as a local server
    /// with one slot.
    pub life: f64,
}

impl Params {
    fn stage(&self, s: &str) -> bool {
        self.birth.iter().any(|b| b == s)
    }
}

/// ``m`` as the model sees it from the moment it arrives. Tool output is shortened (`squeeze`) only when
/// it is the output of a process (`kind_of` its call: a command, an unknown tool): a log repeats itself
/// and its repeats say nothing. What a tool returns of a file or of a search is the material itself
/// (and so is what a command that only prints files returns: `cat`, `sed -n`, `head`, `grep`),
/// where a repeated line is content (an unrolled loop, a row of a table, a run of equal instructions),
/// and it stays as it came: in a measured run, folded repeats in a read of an assembly file cost the
/// answers that depended on them.
/// With `name` (the message's `m#####` number, when the host offers `recall`), a shortened output ends with
/// a line that says how long it was and how to have it back whole: no fold is left without a way back.
fn at_birth(m: &Value, p: &Params, kind_of: &dyn Fn(&str) -> Kind, name: Option<usize>) -> Option<Value> {
    let back = |whole: &str, s: std::borrow::Cow<'_, str>| -> String {
        match name {
            Some(k) => format!(
                "{s}\n{MARK} this output is shortened above ({} lines in full); recall m{k:05} brings it back as it was, or lines from-to of it",
                whole.lines().count()
            ),
            None => s.into_owned(),
        }
    };
    if p.birth.is_empty() {
        return None;
    }
    let content = m.get("content")?;
    let prose =
        p.stage("telegraph") && (m.get("role").and_then(Value::as_str) == Some("user") || p.stage("telegraph-agent"));
    let with = |c: Value| {
        let mut out = m.clone();
        out["content"] = c;
        out
    };
    if let Value::String(s) = content {
        if !prose {
            return None;
        }
        return match compress(s) {
            std::borrow::Cow::Owned(x) if x != *s => Some(with(Value::String(x))),
            _ => None,
        };
    }
    let Value::Array(list) = content else { return None };
    let sq = p.stage("squeeze");
    let process = |o: &Map<String, Value>| {
        matches!(kind_of(o.get("tool_use_id").and_then(Value::as_str).unwrap_or("")), Kind::Exec | Kind::Other)
    };
    let mut out = Vec::with_capacity(list.len());
    let mut changed = false;
    for b in list {
        let mut nb: Option<Value> = None;
        if let Value::Object(o) = b {
            let t = o.get("type").and_then(Value::as_str);
            if t == Some("tool_result") && sq && process(o) {
                match o.get("content") {
                    Some(Value::String(c)) => {
                        let s = squeeze(c, name.is_some());
                        if *s != **c {
                            let mut x = o.clone();
                            x.insert("content".into(), Value::String(back(c, s)));
                            nb = Some(Value::Object(x));
                        }
                    }
                    Some(Value::Array(parts)) => {
                        let mut any = false;
                        let new: Vec<Value> = parts
                            .iter()
                            .map(|x| {
                                if x.get("type").and_then(Value::as_str) == Some("text") {
                                    if let Some(tx) = x.get("text").and_then(Value::as_str) {
                                        let s = squeeze(tx, name.is_some());
                                        if *s != *tx {
                                            any = true;
                                            let mut y = x.clone();
                                            y["text"] = Value::String(back(tx, s));
                                            return y;
                                        }
                                    }
                                }
                                x.clone()
                            })
                            .collect();
                        if any {
                            let mut x = o.clone();
                            x.insert("content".into(), Value::Array(new));
                            nb = Some(Value::Object(x));
                        }
                    }
                    _ => {}
                }
            } else if t == Some("text") && prose {
                if let Some(tx) = o.get("text").and_then(Value::as_str) {
                    let s = compress(tx);
                    if *s != *tx {
                        let mut x = o.clone();
                        x.insert("text".into(), Value::String(s.into_owned()));
                        nb = Some(Value::Object(x));
                    }
                }
            }
        }
        changed |= nb.is_some();
        out.push(nb.unwrap_or_else(|| b.clone()));
    }
    changed.then(|| with(Value::Array(out)))
}

struct Built {
    key: Vec<(usize, usize)>,
    map_ver: u64,
    /// Where each of the message's runs pointed (`dups_on`).
    on: Vec<Option<(usize, usize)>>,
    msg: Option<Value>,
    size: i64,
    /// A hash of the message as built (`pockets`).
    fp: u64,
}

/// The visible messages, by (messages, start, turns) they were worked out for.
type Visible = ((usize, usize, usize), Arc<Vec<usize>>);

mod own;

pub struct Engine {
    p: Params,
    /// Tokens the view may take in the model's window, after the system prompt, the tools and the reply
    /// (0: no limit). The host gives it with each call ([`Engine::set_budget`]).
    budget: i64,
    int: Interner,
    tr: Tracer,
    fz: Featurizer,
    goal: Threads,
    sizes: Vec<i64>,
    raw_tokens: i64,
    dropped: FxHashMap<usize, FxHashSet<usize>>,
    own: own::Own,
    /// The outputs of the request still open that no later agent message drew on, and since when
    /// (`Params.live`).
    live_since: FxHashMap<usize, usize>,
    /// The outputs `live_drop` took out, whole or down to their leads.
    led: FxHashSet<usize>,
    /// The lines an output taken out by `live_drop` keeps: its leads.
    lead_lines: FxHashMap<usize, Vec<String>>,
    /// The parts left out because a later output in the view sends every line of them (result, part).
    copied: FxHashSet<(usize, usize)>,
    /// The parts that send the lines of a part left out as a copy (`copied`): they stay while it is out, or
    /// the stub that says a later output sends them would point to nothing.
    carrying: FxHashSet<(usize, usize)>,
    start: usize,
    map_text: String,
    map_ver: u64,
    gone: IndexMap<String, i64>,
    gone_tokens: i64,
    gone_commands: i64,
    checked: i64,
    checked_at: usize,
    cap: i64,
    floor: f64,
    built: FxHashMap<usize, Built>,
    born: Vec<usize>,
    held: FxHashMap<(usize, usize), f64>,
    held_at: usize,
    pub stats: IndexMap<&'static str, i64>,
    names: Option<Names>,
    pool: Vec<(usize, String, Set, f64)>,
    // what the anchor keeps of the messages that left the view, in their own words (`harvest`)
    findings: Vec<(usize, String)>,
    /// The agent's note lines, as words, with the message each is in, and how many messages were scanned.
    noted: Vec<(usize, FxHashSet<String>)>,
    noted_upto: usize,
    plan: Option<(usize, String)>,
    /// The person's standing instructions: session state, kept for as long as the session lasts.
    rules: Rules,
    /// The anchor without the rules block (the map, the findings, the pinned files and the ledger).
    base_map: String,
    /// The rules block as the anchor has it now.
    rules_shown: String,
    /// file path (`file_key`) -> (read in message, path, text)
    pins: IndexMap<String, (usize, String, String)>,
    /// file path (`file_key`) -> last message that wrote it (a tool or a shell command)
    written: FxHashMap<String, usize>,
    /// file path (`file_key`) -> the agent's last whole write of it, oldest first
    wrote: IndexMap<String, Wrote>,
    /// call id -> the whole writes its write replaced, until its result says whether it wrote the file
    replaced: FxHashMap<String, Vec<Wrote>>,
    /// call id -> what a write tool's call changes (each file, its message, the size of a partial edit), until
    /// its result says whether it changed anything: a failed edit leaves the file as it was
    pending_writes: FxHashMap<String, Vec<(String, usize, Option<usize>)>>,
    /// Agent steps since the last one with a note line, and since the last nudge.
    since_note: usize,
    since_nudge: usize,
    /// The last message with a note line.
    last_note: Option<usize>,
    /// The messages whose last tool result carries the nudge (kept there, so the history stays as sent).
    nudged: FxHashSet<usize>,
    /// The oldest message a note put back to how it was sent before the request was nudged: the view changed
    /// from there, outside `decide`, which counts it.
    unnudged: Option<usize>,
    /// The messages of the view last decided, in order: what the provider cached.
    sent_vis: Arc<Vec<usize>>,
    asked: FxHashSet<Sym>,
    kept_tokens: i64,
    frozen: FxHashMap<usize, String>,
    vis: Option<Visible>,
    last_raw: Value,
    /// Whether the host offers a `note` tool: the request for notes names it, or asks for the lines in the reply.
    note_tool: bool,
    /// Whether the host offers the `recall` tool, which brings a message back as it was by its `m#####` name.
    recall_tool: bool,
    /// The calls that read files, each with its message and its files (kept as the units come, so that
    /// `call_files` does not parse every call again on each request), how many units are in it, and how
    /// many distinct files hold each part of a name.
    calls_raw: Vec<(usize, Vec<Vec<String>>)>,
    sf: crate::flat::StepFiles,
    lead_ix: crate::flat::LeadIndex,
    calls_seen: usize,
    files_known: FxHashSet<Vec<String>>,
    part_files: FxHashMap<String, usize>,
    /// Every distinct line of the tool outputs, as a number (`Params.dedup`).
    line_ids: FxHashMap<String, u32>,
    /// The lines of each tool result as sent: `None` for the ones the result itself sends as a pointer.
    shown: FxHashMap<usize, Vec<Option<u32>>>,
    /// Where each window of lines starts in the tool results (result, line), latest last.
    runs_at: FxHashMap<[u32; 3], Vec<(usize, usize)>>,
    /// The lines already sent as a pointer to an earlier output (`asked=2`).
    pointed_lines: FxHashSet<u32>,
    /// The runs of each message's tool results that an earlier output holds too.
    dups: FxHashMap<usize, Vec<Dup>>,
    /// The runs that point to each tool result, as (message, index in `dups`), in the order they came.
    copies: FxHashMap<usize, Vec<(usize, usize)>>,
    /// The lines each message carries for the steps (`Params.close`): which left, and which came back.
    shut: FxHashMap<usize, Vec<String>>,
    /// The calls and outputs of closed requests: out of the view.
    shut_out: FxHashSet<usize>,
    /// The requests looked at for closing so far.
    shut_done: usize,
    /// The steps that repeat the step before them (`Params.loops`) and are out of the view, the line the
    /// latest of each run carries, how far the messages were looked at, and the run going on: its first
    /// call, its latest call and how many times it ran.
    looped: FxHashSet<usize>,
    loop_line: FxHashMap<usize, String>,
    /// The person's request sent again at the end of an output (`Params.recite`), by message, and the request
    /// the latest of them was for with the count of steps it was due at.
    recited: FxHashMap<usize, String>,
    /// `Params.reruns`: by call, how often it ran, how often its output differed from the run before and the
    /// latest output's hash; and the lines that say so, by message.
    reran: FxHashMap<String, (u32, u32, u64)>,
    rerun_line: FxHashMap<usize, Vec<String>>,
    /// `Params.stall`: by tool result, its message, its lines never seen before and its lines.
    novel: Vec<(usize, usize, usize)>,
    recite_next: (usize, usize),
    loops_seen: usize,
    run: Option<(usize, usize, usize)>,
    /// The steps of the answered requests, in the view or out of it.
    steps: Vec<Step>,
    /// The words of each request, and in how many requests and how many steps each word is: a word in
    /// most of them names nothing in particular.
    requests: Vec<FxHashSet<Sym>>,
    requests_df: FxHashMap<Sym, usize>,
    step_df: FxHashMap<Sym, usize>,
    /// The step each message of the answered requests belongs to (an index into `steps`).
    step_at: FxHashMap<usize, usize>,
    /// The calls that read a file, by the file (`file_key`), and the commands that print files, by the
    /// command: each call's unit and the lines it asked for, oldest first. A call that asks again for what an
    /// earlier one gave, while that output is out of the view, is the agent bringing it back (`asked_back`).
    reads_made: FxHashMap<String, Vec<ReadMade>>,
    /// How many times the agent brought each message back while no step held it yet (its request still
    /// open): the step made of it starts with the count (`Step::back`).
    brought: FxHashMap<usize, u32>,
    /// The edits and writes of files, by a tool or a shell command: (path, message, call id), and whether
    /// each call's result came back without an error (`Params.stale`).
    edits: Vec<(String, usize, String)>,
    edit_ok: FxHashMap<String, bool>,
    /// The files a shell command that only prints files read (`cat`, `sed -n`): by call id while it waits for
    /// its result, then by result unit, so a later edit of one makes the output out of date as a read's.
    shell_read: FxHashMap<String, Vec<String>>,
    shell_reads: FxHashMap<usize, Vec<String>>,
    /// The reads that left the view while a later edit had made them out of date: (path, message of the edit).
    stale_gone: FxHashMap<usize, (String, usize)>,
    /// Seconds, as the host gives them with each view (`Params.pockets`).
    pub now: f64,
    /// The last view sent on each topic, message by message as [`Engine::prints`] gives it, and when.
    pockets: FxHashMap<usize, Pocket>,
    /// The first message of the view the cache no longer holds, while `decide` weighs it (`usize::MAX`: all
    /// of it is held, or new).
    warm: usize,
    /// How long the provider keeps a view, as learned (`learn`): at least `life_lo`, less than `life_hi`
    /// seconds; and whether it ever said it read from its cache.
    life_lo: f64,
    life_hi: f64,
    read_seen: bool,
    /// What the cache gives the view last decided, by how long it keeps views (`learn`).
    probe: Option<Probe>,
    /// The hash of each message as it came (`prints`; 0: not taken yet).
    raw_fp: Vec<u64>,
}

type Proposal = (usize, usize, Vec<usize>, i64);

/// A call that read a file, as `Engine::reads_made` keeps it: its unit and the lines it asked for (from 1,
/// `u64::MAX` to the end), the whole file for a command.
type ReadMade = (usize, (u64, u64));

/// The last view sent on a topic (`Params.pockets`): (message, hash of it as sent), and when.
struct Pocket {
    sent: Vec<(usize, u64)>,
    at: f64,
}

/// The view just decided, weighed against the pockets: the tokens the cache gives it if it keeps the views
/// up to each age, (age, tokens) youngest first, each giving more than the one before; and its tokens.
struct Probe {
    steps: Vec<(f64, i64)>,
    total: i64,
}

/// The pockets kept: a session of many topics drops the oldest.
const POCKETS_KEPT: usize = 16;

/// What the provider says it read tells a pocket's age apart only when the views it would and would not
/// give differ by this share of the request at least; the provider counts in blocks and our tokens are
/// estimates.
const LIFE_GAP: f64 = 0.1;

/// A view the provider did not give at this age or younger tells nothing of its lifetime: a cache keeps
/// views at least this long, and a miss sooner is the provider's own load.
const LIFE_LEAST: f64 = 60.0;

/// A hash of a message as sent, never 0 (0 marks a message sent as it came).
fn fingerprint(v: &Value) -> u64 {
    use std::hash::Hasher;
    struct H(rustc_hash::FxHasher);
    impl std::io::Write for H {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.write(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = H(rustc_hash::FxHasher::default());
    // writing to a hasher does not fail
    let _ = serde_json::to_writer(&mut h, v);
    h.0.finish() | 1
}

/// A worth that orders as a number (the parts [`Engine::cover`] weighs are never NaN).
#[derive(PartialEq)]
struct Score(f64);

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Score {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// A file the agent wrote whole: the message and call that wrote it, its text (capped at
/// [`WROTE_TOKENS`]) and the message of a later edit that made the text stale.
#[derive(Clone, Debug)]
struct Wrote {
    mi: usize,
    call: String,
    path: String,
    text: String,
    edited: Option<usize>,
}

/// A step of an answered request (`Params.close`): a call and its output.
#[derive(Clone, Debug)]
struct Step {
    /// The request message, and the messages of the step.
    a: usize,
    msgs: Vec<usize>,
    /// The distinctive words of its output.
    read: FxHashSet<Sym>,
    /// Those, the ones of its calls, and its topic words (`topics`): what a request may name it by.
    about: FxHashSet<Sym>,
    /// The words its calls named (paths, names, their parts, the topic words of what they did): what it
    /// went for, where `about` also holds what it only came across (a file a listing printed).
    aimed: FxHashSet<Sym>,
    /// What its calls did, for the line that says it left.
    did: Vec<String>,
    out: bool,
    /// The message it was brought back at, as text, and the tokens of that text.
    at: Option<usize>,
    tokens: i64,
    /// The model call since which no request has used it (`None`: in use, or out).
    idle: Option<usize>,
    /// How many times it was brought back: by the curator (`recall`, `named_keep`) or by the agent itself
    /// (`asked_back`).
    back: u32,
}

impl Step {
    /// Where the step is in the view: the message it came back at, or its own first message.
    fn place(&self) -> usize {
        self.at.unwrap_or(self.msgs[0])
    }
}

/// Places a window of output lines is remembered at, the latest ones: a line every output repeats points
/// to the nearest.
const RUN_SOURCES: usize = 32;

/// Lines of a tool output that an earlier output holds too (`Params.dedup`): `stub`, a line that points to
/// them there, goes in their place while that output is whole in the view.
#[derive(Clone, Debug)]
struct Dup {
    /// The tool result and its lines `a..b`.
    unit: usize,
    lines: (usize, usize),
    /// The earlier tool result that holds them, from its line `at`.
    src: usize,
    at: usize,
    stub: String,
}

/// How many lines run `d` stands for.
fn len_of(d: &Dup) -> usize {
    d.lines.1 - d.lines.0
}

/// The line ids a pointer stands for.
fn shown_ids<'a>(ids: &'a [u32], d: &Dup) -> impl Iterator<Item = u32> + 'a {
    ids[d.lines.0..d.lines.1].iter().copied()
}

/// The line a run of `n` lines goes as: the same as lines `j + 1..=j + n` of the output of call `label`.
fn pointer(n: usize, j: usize, label: &str, first: &str, last: &str) -> String {
    format!(
        "{MARK} {n} lines left out here: the same as lines {}-{} of the output of {label} above, from \"{}\" to \"{}\"",
        j + 1,
        j + n,
        clipped(first, 32),
        clipped(last, 32)
    )
}

/// Lines `a..b` of `lines` onto `out`, a run of `runs` among them as its stub, once (`put` holds the runs
/// whose stub is out already).
fn put_lines(lines: &[&str], (a, b): (usize, usize), runs: &[&Dup], put: &mut Vec<usize>, out: &mut Vec<String>) {
    let mut k = a;
    while k < b {
        match runs.iter().position(|d| d.lines.0 <= k && k < d.lines.1) {
            Some(r) => {
                if !put.contains(&r) {
                    put.push(r);
                    out.push(runs[r].stub.clone());
                }
                k = runs[r].lines.1;
            }
            None => {
                out.push(lines[k].to_string());
                k += 1;
            }
        }
    }
}

/// Whether a tool result holds only text: a pointer stands for lines of text, never for an image.
fn text_only(b: &Value) -> bool {
    match b.get("content") {
        Some(Value::Array(parts)) => parts.iter().all(|p| p.get("type").and_then(Value::as_str) == Some("text")),
        _ => true,
    }
}

/// ``s`` cut to ``n`` characters, with "…" when it was longer.
/// How many times `text` says `word` as a whole word.
fn says(text: &str, word: &str) -> usize {
    let alnum = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    text.match_indices(word)
        .filter(|&(k, m)| !alnum(text[..k].chars().next_back()) && !alnum(text[k + m.len()..].chars().next()))
        .count()
}

fn clipped(s: &str, n: usize) -> String {
    if py::len(s) > n {
        format!("{}…", py::head(s, n))
    } else {
        s.to_string()
    }
}

/// The key of a window of lines (its first three; unused places are `u32::MAX`).
fn window(ids: &[u32]) -> [u32; 3] {
    let mut w = [u32::MAX; 3];
    for (x, &id) in w.iter_mut().zip(ids) {
        *x = id;
    }
    w
}

/// ``text`` within ``tokens``: its head and tail lines, and how many lines were cut between them. Lines and
/// characters are weighed as the budget is (`est_str`), so a text dense in quotes or control bytes keeps less.
fn head_and_tail(text: &str, tokens: i64) -> String {
    if est_str(text) <= tokens {
        return text.to_string();
    }
    let half = (tokens.max(2) * 4 / 2) as usize;
    // what a piece adds to `str_chars`, without the two quotes of a string
    let weight = |s: &str| str_chars(s) - 2;
    let fit = |pieces: &mut dyn Iterator<Item = usize>| {
        let (mut used, mut n) = (0, 0);
        for w in pieces {
            used += w;
            if used > half {
                break;
            }
            n += 1;
        }
        n
    };
    let lines: Vec<&str> = text.lines().collect();
    // at least one line goes, or the cut would only add its note
    let head = fit(&mut lines.iter().map(|l| weight(l) + 1)).min(lines.len().saturating_sub(1));
    let tail = fit(&mut lines.iter().rev().map(|l| weight(l) + 1)).min(lines.len().saturating_sub(head + 1));
    if head == 0 && tail == 0 {
        // one long line, or lines each longer than half the budget: the first and last characters that fit
        let chars: Vec<char> = text.chars().collect();
        let mut buf = [0u8; 4];
        let mut w = |c: &char| weight(c.encode_utf8(&mut buf));
        let a = fit(&mut chars.iter().map(&mut w)).min(chars.len().saturating_sub(1));
        let b = fit(&mut chars.iter().rev().map(&mut w)).min(chars.len().saturating_sub(a + 1));
        let n = chars.len();
        let (a_s, b_s): (String, String) = (chars[..a].iter().collect(), chars[n - b..].iter().collect());
        return format!("{a_s}\n… {} characters cut …\n{b_s}", n - a - b);
    }
    format!(
        "{}\n… {} lines cut …\n{}",
        lines[..head].join("\n"),
        lines.len() - head - tail,
        lines[lines.len() - tail..].join("\n")
    )
}

/// ``more`` blocks at the end of message ``m``'s content. A text block left empty (a message that came with
/// no text) goes: providers refuse an empty text block, and what is added says more than it.
fn add_blocks(m: &mut Value, more: impl IntoIterator<Item = Value>) {
    let mut bs = blocks(m.get("content")).into_owned();
    bs.retain(|b| get_str(b, "type") != "text" || !get_str(b, "text").is_empty());
    bs.extend(more);
    m["content"] = Value::Array(bs);
}

/// Whether ``path`` looks like a scratch file ("tmp_out.txt", "_try.rs", "scratch.md", anything under
/// "/tmp/"): the first to go for the budget. A leading underscore alone marks a private, partial or
/// framework file ("_app.tsx", "__init__.py", "_variables.scss"), not a scratch one.
fn scratch(path: &str) -> bool {
    let name = basename(path).to_ascii_lowercase();
    let path = slashes(path).to_ascii_lowercase();
    ["tmp", "_tmp", "_try", "_scratch", "scratch"].iter().any(|m| name.starts_with(m))
        || path.starts_with("/tmp/")
        || path.starts_with("/var/tmp/")
        || ["%temp%/", "%tmp%/", "$env:temp/", "$env:tmp/"].iter().any(|t| path.starts_with(t))
        || path.contains("/appdata/local/temp/")
        || path.contains(":/windows/temp/")
}

/// A Windows path (`C:\proj\a.rs`, `\\host\share\a.rs`, or one with `\` and no `/`, as `src\a.rs`) with `/`
/// for `\`, so the path helpers read it as they read any other; a path with `/` in it as it is.
fn slashes(path: &str) -> std::borrow::Cow<'_, str> {
    let b = path.as_bytes();
    let drive = b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    if path.contains('\\') && (drive || !path.contains('/')) {
        std::borrow::Cow::Owned(path.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

/// Whether a word with `\` in it is a Windows path (`\proj\a.rs`, `src\a.rs`, `..\a.rs`) rather than a
/// pattern with escapes (`a\|b`, `\d+\.rs`, `foo\.txt`): parts of plain name characters, none of which past
/// the first starts with a dot unless it is `.` or `..`.
fn windows_path(w: &str) -> bool {
    w.strip_prefix("\\\\").unwrap_or(w).split('\\').enumerate().all(|(k, s)| {
        (k == 0 || !s.is_empty())
            && s.chars().all(|c| c.is_alphanumeric() || "._-~@".contains(c))
            && (k == 0 || !s.starts_with('.') || s.chars().all(|c| c == '.'))
    })
}

/// The key of a file in the tracked files: its path as written (a Windows one with `/`), with "//" as "/" and
/// without a leading "./".
fn file_key(path: &str) -> String {
    let path = slashes(path);
    let mut key = String::with_capacity(path.len());
    for c in path.chars() {
        if c != '/' || !key.ends_with('/') {
            key.push(c);
        }
    }
    let mut p = key.as_str();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    p.to_string()
}

/// File ``path`` changed in message ``mi`` by an edit of ``size`` characters: the whole writes in ``wrote``
/// of every file it may name (a bare file name names each file of that name), from before, are marked
/// stale, or go when the edit is as big as half of one.
fn edited(wrote: &mut IndexMap<String, Wrote>, path: &str, mi: usize, size: usize) {
    wrote.retain(|key, w| {
        if w.mi >= mi || !same_file(key, path) {
            return true;
        }
        if 2 * size >= w.text.chars().count() {
            return false;
        }
        w.edited = Some(mi);
        true
    });
}

/// The lines of what ``old`` wrote, out of the ledger's pool: the file was written again since, and its old text
/// would read as what the file holds.
fn forget(pool: &mut Vec<(usize, String, Set, f64)>, old: &Wrote) {
    let lines: FxHashSet<String> = old.text.split('\n').map(py::squash_ws).collect();
    pool.retain(|(mi, ln, _, _)| *mi != old.mi || !lines.contains(ln));
}

/// Whether two paths name the same file: equal, or one is the other with more directories in front
/// (`src/a.rs` and `/work/src/a.rs`).
fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (slashes(a), slashes(b));
    let a = a.strip_prefix("./").unwrap_or(&a);
    let b = b.strip_prefix("./").unwrap_or(&b);
    let under = |long: &str, short: &str| long.strip_suffix(short).is_some_and(|h| h.ends_with('/'));
    a == b || under(a, b) || under(b, a)
}

/// The last component of a path, POSIX or Windows ("" for one that ends in a separator).
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or("")
}

impl Engine {
    pub fn new(p: Params) -> Engine {
        let mut goal = Threads::default();
        goal.link = p.link;
        goal.riders = p.task > 0;
        Engine {
            cap: p.cap,
            floor: p.floor,
            p,
            int: Interner::default(),
            tr: Tracer::default(),
            fz: Featurizer::default(),
            goal,
            sizes: Vec::new(),
            raw_tokens: 0,
            dropped: FxHashMap::default(),
            own: own::Own::default(),
            live_since: FxHashMap::default(),
            led: FxHashSet::default(),
            lead_lines: FxHashMap::default(),
            copied: FxHashSet::default(),
            carrying: FxHashSet::default(),
            start: 1,
            map_text: String::new(),
            map_ver: 0,
            gone: IndexMap::new(),
            gone_tokens: 0,
            gone_commands: 0,
            budget: 0,
            checked: 0,
            checked_at: 0,
            built: FxHashMap::default(),
            born: Vec::new(),
            held: FxHashMap::default(),
            held_at: 0,
            stats: IndexMap::new(),
            names: None,
            pool: Vec::new(),
            findings: Vec::new(),
            noted: Vec::new(),
            noted_upto: 0,
            plan: None,
            rules: Rules::default(),
            base_map: String::new(),
            rules_shown: String::new(),
            pins: IndexMap::new(),
            written: FxHashMap::default(),
            wrote: IndexMap::new(),
            replaced: FxHashMap::default(),
            pending_writes: FxHashMap::default(),
            since_note: 0,
            since_nudge: usize::MAX,
            last_note: None,
            nudged: FxHashSet::default(),
            unnudged: None,
            sent_vis: Arc::default(),
            asked: FxHashSet::default(),
            kept_tokens: 0,
            frozen: FxHashMap::default(),
            vis: None,
            last_raw: Value::Null,
            note_tool: true,
            recall_tool: false,
            calls_raw: Vec::new(),
            sf: crate::flat::StepFiles::default(),
            lead_ix: crate::flat::LeadIndex::default(),
            calls_seen: 0,
            files_known: FxHashSet::default(),
            part_files: FxHashMap::default(),
            line_ids: FxHashMap::default(),
            shown: FxHashMap::default(),
            runs_at: FxHashMap::default(),
            pointed_lines: FxHashSet::default(),
            dups: FxHashMap::default(),
            copies: FxHashMap::default(),
            shut: FxHashMap::default(),
            shut_out: FxHashSet::default(),
            shut_done: 0,
            looped: FxHashSet::default(),
            loop_line: FxHashMap::default(),
            recited: FxHashMap::default(),
            reran: FxHashMap::default(),
            rerun_line: FxHashMap::default(),
            novel: Vec::new(),
            recite_next: (usize::MAX, 0),
            loops_seen: 0,
            run: None,
            steps: Vec::new(),
            requests: Vec::new(),
            requests_df: FxHashMap::default(),
            step_df: FxHashMap::default(),
            step_at: FxHashMap::default(),
            reads_made: FxHashMap::default(),
            brought: FxHashMap::default(),
            edits: Vec::new(),
            edit_ok: FxHashMap::default(),
            shell_read: FxHashMap::default(),
            shell_reads: FxHashMap::default(),
            stale_gone: FxHashMap::default(),
            now: 0.0,
            pockets: FxHashMap::default(),
            warm: usize::MAX,
            life_lo: 0.0,
            life_hi: f64::INFINITY,
            read_seen: false,
            probe: None,
            raw_fp: Vec::new(),
        }
    }

    /// The tools the host offers the model (their names). Without a `note` tool, the request for notes asks
    /// for the lines in the reply instead: a request for a tool that is not there is never answered, and
    /// in a measured run it stayed in view for nineteen requests.
    pub fn tools(&mut self, names: &[String]) {
        self.note_tool = names.iter().any(|n| n == NOTE_TOOL);
        self.recall_tool = names.iter().any(|n| n == RECALL_TOOL);
    }

    fn nudge_text(&self) -> &'static str {
        if self.note_tool {
            NUDGE
        } else {
            NUDGE_SAID
        }
    }

    /// Whether the agent is between two parts of one file: its last call read a part of a file (a read
    /// with a range), so the next step is likely the next part.
    fn mid_read(&self, t: usize) -> bool {
        t >= 2
            && blocks(self.tr.msgs[t - 2].get("content")).iter().any(|b| {
                b.get("type").and_then(Value::as_str) == Some("tool_use")
                    && partial_read(get_str(b, "name"), b.get("input"))
            })
    }

    fn bump(&mut self, k: &'static str, v: i64) {
        *self.stats.entry(k).or_insert(0) += v;
    }

    fn add(&mut self, m: &Value) {
        let raw = strip_cc(m);
        // the kind of the call each result answers: the calls are in the message before it; a command
        // that only prints files returns the material, as a read does, and so does a tool given a URL (a page)
        let kinds: FxHashMap<String, Kind> = blocks(self.last_raw.get("content"))
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|b| {
                let (kind, _, command) = classify(get_str(b, "name"), b.get("input"));
                let prints = kind == Kind::Exec && command.as_deref().is_some_and(prints_files);
                let page = kind == Kind::Other && b.get("input").and_then(|x| x.get("url")).is_some();
                (get_str(b, "id").to_string(), if prints || page { Kind::Read } else { kind })
            })
            .collect();
        let name = self.recall_tool.then(|| self.tr.msgs.len() + 1);
        let born = at_birth(&raw, &self.p, &|id| kinds.get(id).copied().unwrap_or(Kind::Other), name);
        let it;
        let msg = match born {
            Some(b) => {
                it = est(&b);
                let saved = (est(&raw) - it).max(0);
                self.bump("birth_saved", saved);
                self.tag_event("birth", saved);
                self.own.born.insert(self.tr.msgs.len());
                b
            }
            None => {
                it = est(&raw);
                if !self.p.birth.is_empty() {
                    self.bump("birth_saved", 0);
                }
                raw.clone()
            }
        };
        if self.p.rules != 0 {
            self.take_rules(&raw);
        }
        if raw.get("role").and_then(Value::as_str) == Some("assistant") {
            let noted = blocks(raw.get("content")).iter().filter_map(note_text).any(|t| NOTE_LINE.is_match(&t));
            self.since_note = if noted { 0 } else { self.since_note + 1 };
            if noted {
                self.last_note = Some(self.tr.msgs.len());
            }
            if noted {
                // the request for notes was answered: the result that carried it goes back to how it was sent
                for old in std::mem::take(&mut self.nudged) {
                    self.built.remove(&old);
                    self.unnudged = Some(self.unnudged.map_or(old, |o| o.min(old)));
                }
            }
            self.since_nudge = self.since_nudge.saturating_add(1);
        }
        self.last_raw = raw;
        self.tr.add(msg, &mut self.int);
        self.sizes.push(it);
        self.raw_tokens += it;
        let new = self.fz.extend(&self.tr, &mut self.int);
        self.track_edits(new.clone());
        self.asked_back(new.clone());
        if self.p.pin != 0 {
            for i in new.clone() {
                let u = &self.tr.units[i];
                if u.kind == UKind::User {
                    self.asked.extend(u.words.iter().copied());
                } else if u.kind == UKind::Result {
                    let id = key_of(self.tr.block(i).get("tool_use_id"));
                    let old = self.replaced.remove(&id).unwrap_or_default();
                    // a whole write that failed did not write the file: what it replaced is still there
                    let failed = self.fz.info.get(&i).is_some_and(|x| x.error && x.kind == Kind::Write);
                    if failed {
                        self.wrote.retain(|_, w| w.call != id);
                        for w in old {
                            self.wrote.insert(file_key(&w.path), w);
                        }
                        self.wrote.sort_by(|_, a, _, b| a.mi.cmp(&b.mi));
                    } else {
                        for w in &old {
                            forget(&mut self.pool, w);
                        }
                    }
                    // what the call changes counts once its result says it did
                    let held = self.pending_writes.remove(&id).unwrap_or_default();
                    if !self.fz.info.get(&i).is_some_and(|x| x.error) {
                        for (path, mi, size) in held {
                            match size {
                                Some(size) => edited(&mut self.wrote, &path, mi, size),
                                None => {
                                    self.written.insert(file_key(&path), mi);
                                }
                            }
                        }
                    }
                } else if u.kind == UKind::Call {
                    let info = self.fz.info.get(&i);
                    let kind = info.map_or(Kind::Other, |x| x.kind);
                    let mut whole: Vec<(String, String)> = Vec::new();
                    if kind == Kind::Write {
                        let b = self.tr.block(i);
                        let path = info.and_then(|x| x.path.clone()).unwrap_or_default();
                        if !basename(&path).is_empty() {
                            match full_write(get_str(b, "name"), b.get("input")) {
                                Some(text) => whole.push((path, text.to_string())),
                                None => {
                                    let size = edit_size(b.get("input"));
                                    let call = key_of(b.get("id"));
                                    self.pending_writes.entry(call).or_default().push((path, u.msg, Some(size)));
                                }
                            }
                        }
                    }
                    let written: Vec<String> = if kind == Kind::Write {
                        info.and_then(|x| x.path.clone()).filter(|p| !p.is_empty()).into_iter().collect()
                    } else if kind == Kind::Exec {
                        let b = self.tr.block(i);
                        shell_writes(
                            b.get("input").and_then(|x| x.get("command")).and_then(Value::as_str).unwrap_or(""),
                        )
                    } else {
                        Vec::new()
                    };
                    for w in written {
                        if kind == Kind::Exec {
                            edited(&mut self.wrote, &w, u.msg, 0);
                            self.written.insert(file_key(&w), u.msg);
                        } else {
                            let call = key_of(self.tr.block(i).get("id"));
                            self.pending_writes.entry(call).or_default().push((w, u.msg, None));
                        }
                    }
                    if kind == Kind::Exec {
                        // a file written whole by a here-document is as much a write as one by a write tool
                        let b = self.tr.block(i);
                        whole.extend(
                            heredoc_writes(&command_of(b)).into_iter().filter(|(p, _)| !basename(p).is_empty()),
                        );
                    }
                    let call = key_of(self.tr.block(i).get("id"));
                    for (path, text) in whole {
                        // the write replaces every earlier whole write of the file; they wait for its result
                        let gone: Vec<String> = self.wrote.keys().filter(|k| same_file(k, &path)).cloned().collect();
                        for k in gone {
                            if let Some(old) = self.wrote.shift_remove(&k) {
                                self.replaced.entry(call.clone()).or_default().push(old);
                            }
                        }
                        let text = head_and_tail(&text, WROTE_TOKENS);
                        let w = Wrote { mi: u.msg, call: call.clone(), path, text, edited: None };
                        self.wrote.insert(file_key(&w.path), w);
                    }
                }
            }
        }
        let k = self.tr.steps.len();
        while self.born.len() < new.end {
            self.born.push(k);
        }
        self.goal.update(&self.tr, &self.fz, MODEL.link.as_ref(), MODEL.link_share);
        self.vis = None;
        if self.p.dedup > 0 {
            self.find_dups(new);
        }
    }

    /// Keeps the edits and writes of files among the units that just arrived (`new`), and whether each
    /// came back without an error (`Params.stale`).
    fn track_edits(&mut self, new: std::ops::Range<usize>) {
        if self.p.stale == 0 {
            return;
        }
        for i in new {
            let u = &self.tr.units[i];
            let info = self.fz.info.get(&i);
            let b = self.tr.block(i);
            if u.kind == UKind::Call {
                let id = key_of(b.get("id"));
                let paths: Vec<String> = match info.map(|x| x.kind) {
                    Some(Kind::Write) => info.and_then(|x| x.path.clone()).into_iter().collect(),
                    Some(Kind::Exec) => {
                        let command = command_of(b);
                        let mut ps = shell_writes(&command);
                        for (p, _) in heredoc_writes(&command) {
                            if !ps.contains(&p) {
                                ps.push(p);
                            }
                        }
                        ps
                    }
                    _ => Vec::new(),
                };
                for p in paths.into_iter().filter(|p| !p.is_empty()) {
                    self.edits.push((p, u.msg, id.clone()));
                }
                if info.is_some_and(|x| x.kind == Kind::Exec) {
                    let read = printed_files(&command_of(b));
                    if !read.is_empty() {
                        self.shell_read.insert(id, read);
                    }
                }
            } else if u.kind == UKind::Result {
                let id = key_of(b.get("tool_use_id"));
                if self.edits.iter().any(|e| e.2 == id) {
                    self.edit_ok.insert(id.clone(), !info.is_some_and(|x| x.error));
                }
                if let Some(read) = self.shell_read.remove(&id) {
                    self.shell_reads.insert(i, read);
                }
            }
        }
    }

    /// The file a read result `i` holds and the message of the first later edit or write of it that did not
    /// fail: the read no longer shows what the file holds (`None`: not a read, or not changed since). A shell
    /// command that only printed files (`shell_reads`) reads them as a read tool does.
    fn superseded_by(&self, i: usize) -> Option<(String, usize)> {
        self.read_superseded(i, false)
    }

    /// As `superseded_by`; with `shell`, a shell command that only printed files counts as a read of them.
    fn read_superseded(&self, i: usize, shell: bool) -> Option<(String, usize)> {
        let u = self.tr.units.get(i)?;
        let info = self.fz.info.get(&i)?;
        if u.kind != UKind::Result || info.error {
            return None;
        }
        let paths: Vec<&str> = match info.kind {
            Kind::Read => info.path.as_deref().filter(|p| !p.is_empty()).into_iter().collect(),
            Kind::Exec if shell => self.shell_reads.get(&i)?.iter().map(String::as_str).collect(),
            _ => return None,
        };
        paths.into_iter().find_map(|path| {
            self.edits
                .iter()
                .find(|(p, mi, id)| *mi > u.msg && same_file(path, p) && self.edit_ok.get(id) == Some(&true))
                .map(|(_, mi, _)| (path.to_string(), *mi))
        })
    }

    /// The runs of the tool results that just arrived (units `new`) that an earlier result, whole in the
    /// view, holds too: each goes as a pointer to it. Decided once, as the message is born, so the message
    /// is sent the same way on every later request (the cache keeps it) for as long as that result stays.
    /// Then the lines the new results send become places later runs can point to.
    fn find_dups(&mut self, new: std::ops::Range<usize>) {
        let least = self.p.dedup as usize;
        let w = least.min(3);
        for i in new {
            let u = &self.tr.units[i];
            if u.kind != UKind::Result || !text_only(self.tr.block(i)) {
                continue;
            }
            let mi = u.msg;
            let text = self.tr.unit_text(i).into_owned();
            let lines: Vec<&str> = text.split('\n').collect();
            let before = self.line_ids.len() as u32;
            let ids: Vec<u32> = lines.iter().map(|l| self.line_id(l.trim_end())).collect();
            if self.p.stall > 0 {
                let full = lines.iter().zip(&ids).filter(|(l, _)| !l.trim().is_empty());
                let (total, new) = full.fold((0, FxHashSet::default()), |(n, mut s), (_, &x)| {
                    if x >= before {
                        s.insert(x);
                    }
                    (n + 1, s)
                });
                self.novel.push((mi, new.len(), total));
            }
            // reruns: how often this call ran and how often its output changed
            let rerun = (self.p.reruns > 0).then(|| {
                let label = self.call_label(i);
                let hash = {
                    use std::hash::{Hash, Hasher};
                    let mut h = rustc_hash::FxHasher::default();
                    ids.hash(&mut h);
                    h.finish()
                };
                let e = self.reran.entry(label.clone()).or_insert((0, 0, hash));
                e.0 += 1;
                let now = e.2 != hash;
                if now {
                    e.1 += 1;
                    e.2 = hash;
                }
                (label, e.0, e.1, now)
            });
            let mut found: Vec<Dup> = Vec::new();
            let mut saved = 0;
            let mut a = 0;
            while a + w <= ids.len() {
                // the longest run from here that an earlier result came with too, the latest: whether it still sends
                // the lines is decided at each request (`dups_on`)
                let mut best = (0, 0, 0);
                for &(s, j) in self.runs_at.get(&window(&ids[a..a + w])).into_iter().flatten().rev() {
                    if self.tr.units[s].msg >= mi {
                        continue;
                    }
                    let src = &self.shown[&s];
                    let n =
                        (0..).take_while(|&n| a + n < ids.len() && src.get(j + n) == Some(&Some(ids[a + n]))).count();
                    if n > best.2 {
                        best = (s, j, n);
                    }
                }
                let (s, j, n) = best;
                if n >= least {
                    let first = lines[a..a + n].iter().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
                    let last = lines[a..a + n].iter().rev().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
                    let stub = pointer(n, j, &self.call_label(s), first, last);
                    let (run, cost) = (est_str(&lines[a..a + n].join("\n")), est_str(&stub));
                    // a pointer is worth it when it is half the lines it stands for or less: below that it saves little and
                    // sends the reader up for them
                    if run >= 2 * cost {
                        saved += run - cost;
                        found.push(Dup { unit: i, lines: (a, a + n), src: s, at: j, stub });
                        a += n;
                        continue;
                    }
                }
                a += 1;
            }
            // an output that is mostly lines already in the view is one the agent asked for again: it did not find or
            // trust the copy above, and a pointer back to it would only send it round once more
            // (with `asked=2` only once a pointer was sent for most of those lines: the first copy asked again
            // goes as a pointer, and the agent asking past it says the pointer did not serve)
            let again = |d: &Dup| shown_ids(&ids, d).filter(|x| self.pointed_lines.contains(x)).count();
            let mostly = 2 * found.iter().map(len_of).sum::<usize>() >= lines.len();
            // (with `asked=3` an output an earlier call gave, line for line, goes as one line that says so, whichever call
            // gave it: the copy above is whole, and when the same call gave it, running it again changed nothing; once
            // that copy leaves the view this one goes whole again, as every pointer does)
            let same = self.p.asked == 3
                && found.len() == 1
                && found[0].lines == (0, lines.len())
                && found[0].at == 0
                && self.shown.get(&found[0].src).map(Vec::len) == Some(lines.len());
            let same_call = same && self.call_label(found[0].src) == self.call_label(i);
            if same {
                let (n, src) = (lines.len(), self.call_label(found[0].src));
                found[0].stub = if same_call {
                    format!("{MARK} the same {n} lines as {src} gave above: nothing changed")
                } else {
                    format!("{MARK} the same {n} lines as {src} gave above")
                };
            }
            let refused = !same
                && (self.p.asked != 2 || 2 * found.iter().map(again).sum::<usize>() >= found.iter().map(len_of).sum());
            if let Some((label, runs, changed, now)) = rerun.filter(|r| r.1 > 1) {
                let fact = match (changed, now) {
                    (0, _) => "the same output each time".to_string(),
                    (c, true) => format!("its output changed {c} of those times, this time too"),
                    (c, false) => format!("its output changed {c} of those times, not this time"),
                };
                let line = format!("{MARK} {label} ran {runs} times in this session: {fact}");
                if same_call && now {
                    // the same as an earlier run, though not as the one right before: it went back
                    found[0].stub =
                        format!("{MARK} the same {} lines as {label} gave above: back to that output", lines.len());
                }
                // on the message itself, so it goes too while the output is sent whole (the newest result is)
                self.rerun_line.entry(mi).or_default().push(line);
                self.built.remove(&mi);
                self.bump("reruns", 1);
            }
            if self.p.asked != 0 && mostly && refused {
                if !found.is_empty() {
                    self.bump("dedup_asked", 1);
                }
                found.clear();
            } else if !found.is_empty() {
                self.bump("dedup_runs", found.len() as i64);
                self.bump("dedup_saved", saved);
                for d in &found {
                    self.pointed_lines.extend(shown_ids(&ids, d));
                }
            }
            let mut shown: Vec<Option<u32>> = ids.into_iter().map(Some).collect();
            for d in &found {
                shown[d.lines.0..d.lines.1].iter_mut().for_each(|x| *x = None);
            }
            for k in 0..(shown.len() + 1).saturating_sub(w) {
                let win: Option<Vec<u32>> = shown[k..k + w].iter().copied().collect();
                if let Some(win) = win {
                    let at = self.runs_at.entry(window(&win)).or_default();
                    if at.len() == RUN_SOURCES {
                        at.remove(0);
                    }
                    at.push((i, k));
                }
            }
            self.shown.insert(i, shown);
            if !found.is_empty() {
                let ds = self.dups.entry(mi).or_default();
                for d in found {
                    self.copies.entry(d.src).or_default().push((mi, ds.len()));
                    ds.push(d);
                }
            }
        }
    }

    fn line_id(&mut self, line: &str) -> u32 {
        if let Some(&id) = self.line_ids.get(line) {
            return id;
        }
        let id = self.line_ids.len() as u32;
        self.line_ids.insert(line.to_string(), id);
        id
    }

    /// The call whose output is tool result `i`, as a pointer to that output names it: its command, or its
    /// tool and arguments, on one line.
    fn call_label(&self, i: usize) -> String {
        let u = &self.tr.units[i];
        let id = key_of(self.tr.block(i).get("tool_use_id"));
        let call = (u.msg.saturating_sub(3)..u.msg)
            .rev()
            .flat_map(|m| self.fz.by_msg(m).iter().copied())
            .find(|&c| self.tr.units[c].kind == UKind::Call && key_of(self.tr.block(c).get("id")) == id);
        let Some(c) = call else { return format!("the {} call", u.tool) };
        let b = self.tr.block(c);
        let command = command_of(b);
        let text = if command.is_empty() {
            format!("{} {}", get_str(b, "name"), b.get("input").map_or(String::new(), Value::to_string))
        } else {
            command
        };
        format!("`{}`", clipped(&py::squash_ws(&text), 100))
    }

    /// The lines of tool result `i` the view sends: none when its message is not visible, else all but those
    /// of its cut parts.
    fn sent_lines(&self, i: usize, vis: &[usize]) -> Vec<bool> {
        let n = self.shown.get(&i).map_or(0, Vec::len);
        if vis.binary_search(&self.tr.units[i].msg).is_err() {
            return vec![false; n];
        }
        let mut sent = vec![true; n];
        if let Some(drop) = self.dropped.get(&i) {
            let at = |x: i64| (if x < 0 { (n as i64 + x).max(0) } else { x.min(n as i64) }) as usize;
            for (_, c) in self.fz.chunks[i].iter().enumerate().filter(|(j, _)| drop.contains(j)) {
                sent[at(c.start)..at(c.end).max(at(c.start))].iter_mut().for_each(|s| *s = false);
            }
        }
        sent
    }

    /// Whether tool result `u`, from its line `x`, sends every line of run `d` that the run's own message
    /// sends (`mine`): then the run can point there and the person's view loses nothing.
    fn covers(&self, d: &Dup, mine: &[bool], (u, x): (usize, usize), vis: &[usize]) -> bool {
        let theirs = self.sent_lines(u, vis);
        (0..len_of(d)).all(|k| !mine[d.lines.0 + k] || theirs.get(x + k) == Some(&true))
    }

    /// Where each run of message `mi` points now, as (tool result, line): to its earlier output while that
    /// output sends its lines, else to the first copy of them that does (`carrier`). `None`: the run is sent
    /// as it came.
    fn dups_on(&mut self, mi: usize) -> Vec<Option<(usize, usize)>> {
        let Some(n) = self.dups.get(&mi).map(Vec::len) else { return Vec::new() };
        if self.p.fresh != 0 && mi + 1 == self.tr.msgs.len() {
            return vec![None; n];
        }
        let vis = self.visible();
        let ds = &self.dups[&mi];
        ds.iter()
            .enumerate()
            .map(|(k, d)| {
                let mine = self.sent_lines(d.unit, &vis);
                if self.covers(d, &mine, (d.src, d.at), &vis) {
                    Some((d.src, d.at))
                } else {
                    self.holder(d, &mine, &vis).or_else(|| self.carrier(d, &mine, (mi, k), &vis))
                }
            })
            .collect()
    }

    /// The first earlier tool result that came with the lines of run `d` itself, in the same order, and
    /// still sends every one of them its message sends (`mine`): a read made again after its first output
    /// lost some chunks, by the same or another command, then holds them. Found by content, not by command.
    fn holder(&self, d: &Dup, mine: &[bool], vis: &[usize]) -> Option<(usize, usize)> {
        let w = (self.p.dedup as usize).min(3);
        let ids: Vec<u32> = self.shown[&d.src][d.at..d.at + len_of(d)].iter().map(|x| x.unwrap_or(u32::MAX)).collect();
        let me = self.tr.units[d.unit].msg;
        let mut found: Vec<(usize, usize)> = self
            .runs_at
            .get(&window(&ids[..w.min(ids.len())]))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&(s, j)| {
                self.tr.units[s].msg < me
                    && self.shown[&s]
                        .get(j..j + ids.len())
                        .is_some_and(|l| l.iter().zip(&ids).all(|(a, b)| *a == Some(*b)))
                    && self.covers(d, mine, (s, j), vis)
            })
            .collect();
        found.sort_unstable();
        found.first().copied()
    }

    /// Where run `d` (`at` in `dups`) points when the output it points to no longer sends its lines: the
    /// copies of that output before it go in full in order, each one but those an earlier one in full or the
    /// output itself covers, and the first of them that covers it carries its lines. Lines read many times
    /// then go once after their first output leaves, not once per copy.
    fn carrier(&self, d: &Dup, mine: &[bool], at: (usize, usize), vis: &[usize]) -> Option<(usize, usize)> {
        let within = |f: &Dup, e: &Dup| f.unit != e.unit && f.at <= e.at && e.at + len_of(e) <= f.at + len_of(f);
        let line = |f: &Dup, e: &Dup| (f.unit, f.lines.0 + e.at - f.at);
        let mut full: Vec<&Dup> = Vec::new();
        for &(m, k) in &self.copies[&d.src] {
            if (m, k) == at {
                break;
            }
            // a copy out of the view sends nothing, so it carries nothing: no need to weigh its lines
            if vis.binary_search(&m).is_err() {
                continue;
            }
            let e = &self.dups[&m][k];
            let theirs = self.sent_lines(e.unit, vis);
            let carried = !theirs[e.lines.0..e.lines.1].contains(&true)
                || self.covers(e, &theirs, (e.src, e.at), vis)
                || full.iter().any(|f| within(f, e) && self.covers(e, &theirs, line(f, e), vis));
            if !carried {
                full.push(e);
            }
        }
        full.iter().find(|f| within(f, d) && self.covers(d, mine, line(f, d), vis)).map(|f| line(f, d))
    }

    /// The pointer run `d` goes as when it points to line `x` of tool result `u`.
    fn stub_at(&self, d: &Dup, (u, x): (usize, usize)) -> String {
        if (u, x) == (d.src, d.at) {
            return d.stub.clone();
        }
        let text = self.tr.unit_text(d.unit);
        let run: Vec<&str> = text.split('\n').skip(d.lines.0).take(len_of(d)).collect();
        let first = run.iter().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
        let last = run.iter().rev().find(|l| !l.trim().is_empty()).map_or("", |l| l.trim());
        pointer(len_of(d), x, &self.call_label(u), first, last)
    }

    /// The rules in message ``m`` as it arrives (before it is cut, and whatever topic it belongs to):
    /// the lines of the person's text that read as one, and the agent's lines beginning with `Rule:` (in its
    /// text or in a `note` call).
    fn take_rules(&mut self, m: &Value) {
        let mi = self.tr.msgs.len();
        let bs = blocks(m.get("content"));
        let texts: Vec<String> = bs.iter().filter_map(note_text).collect();
        let text = texts.join("\n");
        match m.get("role").and_then(Value::as_str) {
            Some("user") => {
                // a turn is a message the person wrote, not one that carries tool results back
                let results = bs.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
                if !results && !text.trim().is_empty() {
                    self.rules.turn += 1;
                }
                for r in rules_in(&text) {
                    self.rules.add(mi, r, false);
                }
            }
            Some("assistant") => {
                for c in NOTE_LINE.captures_all(&text) {
                    let span = |g: usize| c[g].map_or("", |(x, y)| &text[x..y]);
                    if span(1) == "Rule" {
                        self.rules.add(mi, clip(&py::squash_ws(span(2))), true);
                    }
                }
            }
            _ => {}
        }
    }

    /// Takes in the conversation as the harness sends it: new messages are added; an edited history
    /// starts the engine over, with the rules of the session carried over.
    pub fn sync(&mut self, msgs: &[Value]) {
        let n = self.tr.msgs.len();
        let rewritten = n > 0 && (msgs.len() < n || !py_eq(&strip_cc(&msgs[n - 1]), &self.last_raw));
        if rewritten {
            let rules = std::mem::take(&mut self.rules).carried();
            // what the host set (window, tools, time) is not part of the history, and neither is what was
            // learned of the provider: it survives the restart
            let (budget, note_tool, recall_tool) = (self.budget, self.note_tool, self.recall_tool);
            let (now, life, read_seen) = (self.now, (self.life_lo, self.life_hi), self.read_seen);
            *self = Engine::new(self.p.clone());
            self.rules = rules;
            self.budget = budget;
            self.note_tool = note_tool;
            self.recall_tool = recall_tool;
            (self.now, (self.life_lo, self.life_hi), self.read_seen) = (now, life, read_seen);
        }
        // a rewritten history is read anew: the rules found in it are the carried ones, where they were;
        // what the person wrote after the last reply comes with the rewrite and is new; with no reply at all
        // (a summary), only the newest message is, and a rewrite of one message is all summary
        let n = self.tr.msgs.len();
        let last_reply = msgs.iter().rposition(|m| m.get("role").and_then(Value::as_str) == Some("assistant"));
        let fresh_from = last_reply.map_or(msgs.len().saturating_sub(1).max(1), |r| r + 1);
        for (k, m) in msgs.iter().enumerate().skip(n) {
            self.rules.replay = rewritten && k < fresh_from;
            self.add(m);
        }
        self.rules.replay = false;
    }

    fn visible(&mut self) -> Arc<Vec<usize>> {
        let key = (self.tr.msgs.len(), self.start, self.goal.turns.len());
        if let Some((k, v)) = &self.vis {
            if *k == key {
                return v.clone();
            }
        }
        let g = &self.goal;
        let apart = self.apart();
        let out: Vec<usize> = if g.turns.is_empty() {
            (0..key.0).filter(|mi| !self.looped.contains(mi)).collect()
        } else if !apart {
            (0..key.0)
                .filter(|mi| {
                    *mi == 0 || (*mi >= self.start && !self.shut_out.contains(mi) && !self.looped.contains(mi))
                })
                .collect()
        } else {
            let mut out = vec![0];
            for &j in &g.members[g.active()] {
                let (a, b) = g.span(j, key.0);
                out.extend(
                    (a.max(self.start).max(1)..b).filter(|mi| !self.shut_out.contains(mi) && !self.looped.contains(mi)),
                );
            }
            out
        };
        let out = Arc::new(out);
        self.vis = Some((key, out.clone()));
        out
    }

    fn key(&self, mi: usize) -> Vec<(usize, usize)> {
        self.fz.by_msg(mi).iter().filter_map(|i| self.dropped.get(i).map(|d| (*i, d.len()))).collect()
    }

    /// Message ``mi`` as sent now and its tokens (`None` when it is sent as it arrived).
    fn message(&mut self, mi: usize) -> (Option<&Value>, i64) {
        let key = self.key(mi);
        let on = self.dups_on(mi);
        let map_ver = if mi == 0 { self.map_ver } else { 0 };
        let plain = key.is_empty()
            && (mi != 0 || self.map_text.is_empty())
            && !self.frozen.contains_key(&mi)
            && !self.shut.contains_key(&mi)
            && !self.loop_line.contains_key(&mi)
            && !self.recited.contains_key(&mi)
            && !self.rerun_line.contains_key(&mi)
            && !self.nudged.contains(&mi)
            && on.iter().all(Option::is_none);
        if plain {
            return (None, self.sizes[mi]);
        }
        let fresh = self.built.get(&mi).is_some_and(|b| b.key == key && b.map_ver == map_ver && b.on == on);
        if !fresh {
            let (msg, size) = self.build(mi, !key.is_empty(), &on);
            let fp = fingerprint(&msg);
            self.built.insert(mi, Built { key, map_ver, on, msg: Some(msg), size, fp });
        }
        let b = &self.built[&mi];
        (b.msg.as_ref(), b.size)
    }

    /// Message `mi` as sent now: its cut parts left out, and its runs that `on` marks as pointers.
    fn build(&self, mi: usize, cut: bool, on: &[Option<(usize, usize)>]) -> (Value, i64) {
        let m = &self.tr.msgs[mi];
        let mut m2 = m.clone();
        let runs: Vec<Dup> = match self.dups.get(&mi) {
            Some(ds) => ds
                .iter()
                .zip(on)
                .filter_map(|(d, p)| p.map(|p| Dup { stub: self.stub_at(d, p), ..d.clone() }))
                .collect(),
            None => Vec::new(),
        };
        if let (true, Some(Value::Array(content))) = (cut || !runs.is_empty(), m.get("content")) {
            let mut new = Vec::new();
            for (bi, b) in content.iter().enumerate() {
                let Some(&i) = self.tr.at.get(&(mi, bi)) else {
                    new.push(b.clone());
                    continue;
                };
                let mine: Vec<&Dup> = runs.iter().filter(|d| d.unit == i).collect();
                match self.dropped.get(&i) {
                    Some(d) => {
                        if let Some(nb) = self.shrink(i, d, b, &mine) {
                            new.push(nb);
                        }
                    }
                    None if !mine.is_empty() => {
                        let text = self.tr.unit_text(i);
                        let lines: Vec<&str> = text.split('\n').collect();
                        let mut out = Vec::new();
                        put_lines(&lines, (0, lines.len()), &mine, &mut Vec::new(), &mut out);
                        let mut nb = b.clone();
                        nb["content"] = Value::String(out.join("\n"));
                        new.push(nb);
                    }
                    None => new.push(b.clone()),
                }
            }
            if !new.is_empty() {
                m2["content"] = Value::Array(new);
            }
        }
        let nudge = self.nudge_text();
        if self.nudged.contains(&mi) {
            if let Some(Value::Array(content)) = m2.get_mut("content") {
                let last =
                    content.iter_mut().rev().find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
                if let Some(b) = last {
                    match b.get_mut("content") {
                        Some(Value::Array(parts)) => parts.push(text_block(nudge)),
                        Some(Value::String(s)) => *s = format!("{s}\n\n{nudge}"),
                        _ => b["content"] = Value::String(nudge.to_string()),
                    }
                }
            }
        }
        if mi == 0 && !self.map_text.is_empty() {
            add_blocks(&mut m2, [text_block(&self.map_text)]);
        }
        if let Some(lines) = self.shut.get(&mi) {
            add_blocks(&mut m2, lines.iter().map(|l| text_block(l)));
        }
        if let Some(line) = self.loop_line.get(&mi) {
            add_blocks(&mut m2, [text_block(line)]);
        }
        if let Some(line) = self.recited.get(&mi) {
            add_blocks(&mut m2, [text_block(line)]);
        }
        if let Some(lines) = self.rerun_line.get(&mi) {
            add_blocks(&mut m2, lines.iter().map(|l| text_block(l)));
        }
        let mut size = est(&m2);
        if let Some(card) = self.frozen.get(&mi) {
            let tb = text_block(card);
            size += est(&tb);
            add_blocks(&mut m2, [tb]);
        }
        (m2, size)
    }

    /// Unit `i` (block `b`) without the chunks in `drop`; the runs of `runs` in what is kept go as pointers.
    fn shrink(&self, i: usize, drop: &FxHashSet<usize>, b: &Value, runs: &[&Dup]) -> Option<Value> {
        let u = &self.tr.units[i];
        let chunks = &self.fz.chunks[i];
        if u.kind == UKind::Thinking {
            return None;
        }
        let mut out = b.clone();
        if u.kind == UKind::Call {
            if let Some(Value::Object(args)) = b.get("input") {
                // only a write leaves its text in a file to read back; a command or a prompt does not
                let wrote = classify(get_str(b, "name"), b.get("input")).0 == Kind::Write;
                // the text an edit replaced is gone from the file: saying the file holds it would mislead
                let args: Map<String, Value> = args
                    .iter()
                    .map(|(k, v)| {
                        let what = match (wrote, REPLACED_ARGS.contains(&k.as_str())) {
                            (true, true) => "this edit replaced here are omitted; they are no longer in the file",
                            (true, false) => "written here are omitted; the file on disk is current",
                            _ => "of this argument here are omitted",
                        };
                        let v = match v {
                            Value::String(s) if py::len(s) > 200 => {
                                Value::String(format!("{MARK} {} lines {what}", s.matches('\n').count() + 1))
                            }
                            _ => v.clone(),
                        };
                        (k.clone(), v)
                    })
                    .collect();
                out["input"] = Value::Object(args);
            }
            return Some(out);
        }
        let text = text_of(b.get("content"));
        let lines: Vec<&str> = text.split('\n').collect();
        let copy = |j: usize| self.copied.contains(&(i, j));
        if drop.len() == chunks.len() && drop.iter().all(|&j| copy(j)) {
            out["content"] = Value::String(format!(
                "{MARK} {} output left out here: a later output below sends every line of it",
                u.tool
            ));
            return Some(out);
        }
        if drop.len() == chunks.len() {
            let mut stub = if let Some((path, edit)) = self.stale_gone.get(&i) {
                // a read the file's later edit made out of date: which file, which edit, and the way back
                let back = if self.recall_tool {
                    format!("recall m{:05} brings it back as it was", u.msg + 1)
                } else {
                    "read the file again to see it as it is now".to_string()
                };
                format!(
                    "{MARK} {} output left out here: the read of {path} from before its edit in m{:05}; {back}",
                    u.tool,
                    edit + 1
                )
            } else if self.recall_tool {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already; recall m{:05} brings it back as it was",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" },
                    u.msg + 1
                )
            } else if self.p.brief != 0 {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already; run it again to see it again",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "{MARK} {} output left out here, {} line{} you read already, as not needed for this step; if the step does need it, run the tool again and it comes back in full",
                    u.tool,
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" }
                )
            };
            if let Some(lines) = self.lead_lines.get(&i) {
                stub.push_str("; its lines that name what nothing has followed yet:");
                for l in lines {
                    stub.push('\n');
                    stub.push_str(l);
                }
            }
            out["content"] = Value::String(stub);
            return Some(out);
        }
        let mut kept: Vec<String> = Vec::new();
        let mut put = Vec::new();
        let n = lines.len() as i64;
        let at = |x: i64| (if x < 0 { (n + x).max(0) } else { x.min(n) }) as usize;
        for (j, c) in chunks.iter().enumerate() {
            if drop.contains(&j) {
                let mark = if copy(j) {
                    format!("{MARK} … lines omitted: a later output below sends them …")
                } else {
                    format!("{MARK} … lines omitted …")
                };
                if kept.last() != Some(&mark) {
                    kept.push(mark);
                }
            } else {
                let (a, b) = (at(c.start), at(c.end));
                if a < b {
                    put_lines(&lines, (a, b), runs, &mut put, &mut kept);
                }
            }
        }
        out["content"] = Value::String(kept.join("\n"));
        Some(out)
    }

    fn sizes_now(&mut self) -> Vec<i64> {
        let mut out = vec![0; self.tr.msgs.len()];
        for &mi in self.visible().iter() {
            out[mi] = self.message(mi).1;
        }
        out
    }

    /// For each message, the tokens its outputs send in place of the copies that point to them from the
    /// messages `stays` keeps: were it to leave, they would go in those copies instead (`dups_on`), so
    /// leaving does not save them.
    fn moved(&mut self, stays: impl Fn(usize) -> bool) -> FxHashMap<usize, i64> {
        let mut out: FxHashMap<usize, i64> = FxHashMap::default();
        for (u, lines) in self.pointed(stays) {
            let held = lines.iter().filter(|x| **x).count() as i64;
            *out.entry(self.tr.units[u].msg).or_default() +=
                self.tr.units[u].tokens * held / (lines.len() as i64).max(1);
        }
        out
    }

    /// The runs of the messages `vis` sent as a pointer now: (tool result, its lines `a..b`).
    fn pointing(&mut self, vis: &[usize]) -> Vec<(usize, (usize, usize))> {
        let mut out = Vec::new();
        for &mi in vis {
            let on = self.dups_on(mi);
            for (d, at) in self.dups.get(&mi).into_iter().flatten().zip(on) {
                if at.is_some() {
                    out.push((d.unit, d.lines));
                }
            }
        }
        out
    }

    /// For each output, its lines that a copy in a visible message `stays` keeps points to now: were the
    /// output to stop sending them, that copy would send them instead.
    fn pointed(&mut self, stays: impl Fn(usize) -> bool) -> FxHashMap<usize, Vec<bool>> {
        let mut out: FxHashMap<usize, Vec<bool>> = FxHashMap::default();
        if self.p.dedup == 0 {
            return out;
        }
        let vis = self.visible();
        let mut holders: Vec<usize> = self.dups.keys().copied().filter(|m| stays(*m)).collect();
        holders.retain(|m| vis.binary_search(m).is_ok());
        for mi in holders {
            let on = self.dups_on(mi);
            for (d, at) in self.dups[&mi].iter().zip(on) {
                if let Some((u, x)) = at {
                    let n = self.shown.get(&u).map_or(0, Vec::len);
                    let lines = out.entry(u).or_insert_with(|| vec![false; n]);
                    lines.iter_mut().skip(x).take(len_of(d)).for_each(|l| *l = true);
                }
            }
        }
        out
    }

    pub fn tokens(&mut self) -> i64 {
        self.sizes_now().iter().sum()
    }

    /// The view: the visible messages, consecutive ones of the same role joined.
    pub fn render(&mut self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for &mi in self.visible().iter() {
            let m = match self.message(mi).0 {
                Some(v) => v.clone(),
                None => self.tr.msgs[mi].clone(),
            };
            if let Some(prev) = out.last_mut() {
                if prev.get("role") == m.get("role") {
                    let mut bs = blocks(prev.get("content")).into_owned();
                    bs.extend(blocks(m.get("content")).iter().cloned());
                    prev["content"] = Value::Array(bs);
                    continue;
                }
            }
            out.push(m);
        }
        out
    }

    /// For measurement: (first message, thread) of each turn so far.
    pub fn turn_threads(&self) -> Vec<(usize, usize)> {
        self.goal.assignment()
    }

    pub fn view(&mut self) -> Vec<Value> {
        self.decide();
        self.render()
    }

    /// Whether the other topics leave the view: always, unless `Params.apart` asks them to hold that many
    /// tokens together first. Below it the whole conversation stays, as without topics.
    fn apart(&self) -> bool {
        if self.p.apart <= 0 || self.goal.turns.is_empty() {
            return true;
        }
        let g = &self.goal;
        let n = self.tr.msgs.len();
        let on: i64 = g.members[g.active()]
            .iter()
            .map(|&j| {
                let (a, b) = g.span(j, n);
                self.sizes[a.max(1)..b].iter().sum::<i64>()
            })
            .sum();
        let rest: i64 = self.sizes[1.min(n)..].iter().sum::<i64>() - on;
        rest >= self.p.apart
    }

    fn cards_text(&self) -> String {
        let g = &self.goal;
        let others = g.others();
        if others.is_empty() || !self.apart() {
            return String::new();
        }
        let n = self.tr.msgs.len();
        let mut fixes = |j: usize| {
            let (a, b) = g.span(j, n);
            self.fixes(a, b, Some(n))
        };
        let mut lines = vec![format!(
            "{MARK} This conversation has other topics. They are left out so that this request gets the context of its own goal; anything in them can be asked for or looked up again:"
        )];
        let shown = head_slice(others.len(), self.p.cards);
        for &k in &others[..shown] {
            let mut card = g.card(&self.tr, &self.fz, k, &mut fixes);
            if self.recall_tool && !card.is_empty() {
                // where the topic's messages are, so `recall` brings any of them back as they were
                let mut runs: Vec<(usize, usize)> = Vec::new();
                for &j in &g.members[k] {
                    let (a, b) = g.span(j, n);
                    match runs.last_mut() {
                        Some(r) if r.1 == a => r.1 = b,
                        _ => runs.push((a, b)),
                    }
                }
                let named: Vec<String> = runs
                    .iter()
                    .filter(|r| r.1 > r.0)
                    .map(
                        |&(a, b)| {
                            if b - a == 1 {
                                format!("m{:05}", a + 1)
                            } else {
                                format!("m{:05}-m{:05}", a + 1, b)
                            }
                        },
                    )
                    .collect();
                if !named.is_empty() {
                    card += &format!(" [recall brings any of {} back as it was]", named.join(", "));
                }
            }
            lines.push(card);
        }
        if shown < others.len() {
            lines.push(format!("• … and {} older topics.", others.len() - shown));
        }
        lines.join("\n")
    }

    pub fn decide(&mut self) {
        self.own.clock += 1;
        self.index_calls();
        // what the view sent before: a message of it that a step below changes without weighing the cache
        // (it changes what the view sends, not what is weighed) still counts in `miss`
        let sent = self.sent_vis.clone();
        let was_sent = |mi: usize| sent.contains(&mi);
        let mut quiet: Vec<(&'static str, usize)> = Vec::new();
        if let Some(mi) = self.unnudged.take().filter(|&mi| was_sent(mi)) {
            quiet.push(("miss_note", mi));
        }
        // the output that went whole as the latest message (`Params.fresh`) points up from now on
        if let Some(&mi) = sent.last().filter(|&&mi| self.p.fresh != 0 && self.dups.contains_key(&mi)) {
            if mi + 1 < self.tr.msgs.len() && self.dups_on(mi).iter().any(Option::is_some) {
                quiet.push(("miss_fresh", mi));
            }
        }
        if self.p.loops != 0 {
            if let Some(mi) = self.fold_loops().filter(|&mi| was_sent(mi)) {
                quiet.push(("miss_loops", mi));
            }
        }
        // the provider's cache holds the last view of each topic for a while (`Params.pockets`): where it
        // stops in the view as it stands, which cuts past it do not move
        let (warm, warm_all, cold) =
            if self.p.pockets != 0 { self.warm_now() } else { (usize::MAX, usize::MAX, false) };
        self.warm = warm;
        let nt = self.goal.turns.len();
        // the oldest message closing steps changed in this request: the cache misses from there on
        let mut closed: Option<usize> = None;
        // a history with no request from the person yet has nothing to close, idle or bring back
        if self.p.close != 0 && nt > 0 {
            let (now, mut turned) = (self.tr.steps.len(), false);
            while self.shut_done + 1 < nt {
                if let Some(unused) = self.close_unused(self.shut_done, nt - 1) {
                    for (i, s) in self.steps.iter_mut().enumerate().filter(|(_, s)| !s.out) {
                        s.idle = if unused.contains(&i) { Some(s.idle.unwrap_or(now)) } else { None };
                    }
                }
                self.shut_done += 1;
                turned = true;
            }
            if self.p.topic != 0 {
                // the steps of the topic the person is on stay: the next request on it works from them
                let on = self.goal.active();
                let thread: FxHashMap<usize, usize> = self.goal.turns.iter().map(|t| (t.start, t.thread)).collect();
                for s in self.steps.iter_mut().filter(|s| thread.get(&s.a) == Some(&on)) {
                    s.idle = None;
                }
            }
            if self.p.noted != 0 {
                self.noted_close(now);
            }
            if self.p.live != 0 {
                if let Some(mi) = self.live_drop(now) {
                    quiet.push(("miss_live", mi));
                }
            }
            // what the latest request asks is weighed against every step, those of the request just closed too
            let need = self.asks(nt - 1);
            let named_back = self.p.named != 0 && self.named_keep(turned);
            if (turned && self.recall(&need)) || named_back {
                closed = self.shut_lines();
            }
            let idle: Vec<usize> = (0..self.steps.len()).filter(|&i| self.steps[i].idle.is_some()).collect();
            if !idle.is_empty() && self.rent_close(idle) {
                if let Some(mi) = self.shut_lines() {
                    closed = Some(closed.map_or(mi, |c| c.min(mi)));
                }
            }
        }
        let mut switched = false;
        if nt > 1 && self.goal.turns[nt - 1].thread != self.goal.turns[nt - 2].thread {
            let start = self.goal.turns[nt - 1].start;
            if !self.frozen.contains_key(&start) {
                let text = self.cards_text();
                if !text.is_empty() {
                    self.frozen.insert(start, text);
                    self.bump("cards", 1);
                }
                self.bump("switches", 1);
                switched = true;
                // the view sends the messages of another topic: the cache misses from the first that differs
                // from what it holds (with pockets, the last view of that topic, if it lives)
                if self.p.pockets != 0 {
                    if warm != usize::MAX {
                        quiet.push(("miss_switch", warm));
                    }
                } else {
                    let vis = self.visible();
                    // (the message before it joins it when they are of the same role)
                    if let Some(p) = (0..sent.len()).find(|&p| vis.get(p) != Some(&sent[p])) {
                        let role = |m: usize| self.tr.msgs[m].get("role").and_then(Value::as_str);
                        let joined = p > 0 && vis.get(p).is_some_and(|&m| role(m) == role(vis[p - 1]));
                        quiet.push((
                            "miss_switch",
                            if joined { vis[p - 1] } else { vis.get(p).map_or(sent[p], |&m| m.min(sent[p])) },
                        ));
                    }
                }
            }
        }
        // the same topic, but its last view outlived the cache: what a cache that kept it would hold is read
        // again
        if warm < warm_all {
            self.bump("pocket_cold", 1);
            if !switched {
                quiet.push(("miss_cold", warm));
            }
        }
        let t = self.tr.msgs.len();
        if t == 0 {
            self.warm = usize::MAX;
            return;
        }
        let k = self.tr.steps.len();
        let mut sizes = self.sizes_now();
        let total: i64 = sizes.iter().sum();
        // where the cache stops, the view is read again whatever is cut: what is worth cutting is weighed now
        let due = warm != usize::MAX
            || total - self.checked >= self.p.check
            || k as i64 - self.checked_at as i64 >= self.p.every;
        let held = if self.p.hold != 0 && self.p.eoq != 0 { self.held(&sizes) } else { 0 };
        if self.p.eoq != 0 {
            // what no cut takes: the first message and the protected tail
            let tail = (t as i64 - self.p.protect).clamp(1, t as i64) as usize;
            let base = (sizes[0] + sizes[tail..].iter().sum::<i64>() - held).max(0);
            let stuck;
            (self.cap, self.floor, stuck) = self.eoq_cap(k, base);
            if stuck {
                self.bump("cap_stuck", 1);
            }
        }
        let mut first: Option<usize> = None;
        let capped = self.cap != 0 && total - held > self.cap;
        // a view the cache holds no more of than its anchor is read again whole: it is cut down to what the
        // cap leaves now, for free, rather than later at the price of breaking the cache (`Params.pockets` 2)
        let squeeze = self.p.pockets >= 2
            && !capped
            && cold
            && self.cap != 0
            && (total - held) as f64 > self.floor * self.cap as f64;
        let mut over = capped || squeeze;
        if over && self.p.cut_copies {
            let target = self.floor * self.cap as f64 + held as f64;
            if target != 0.0 {
                first = self.need(t, &sizes, target, true);
                sizes = self.sizes_now();
            }
            let left = sizes.iter().sum::<i64>();
            over = left - held > 2 * self.cap || (squeeze && left as f64 > target);
        }
        if over {
            if let Some(e) = self.evict(t, k, &sizes, true) {
                first = Some(first.map_or(e, |f| f.min(e)));
            }
            sizes = self.sizes_now();
            let target = self.floor * self.cap as f64 + held as f64;
            if self.p.cut_cover && target != 0.0 && sizes.iter().sum::<i64>() as f64 > target {
                if let Some(mi) = self.cover(t, &sizes, target) {
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                    sizes = self.sizes_now();
                }
            }
            if self.p.cut_need && target != 0.0 && sizes.iter().sum::<i64>() as f64 > target {
                if let Some(mi) = self.need(t, &sizes, target, false) {
                    first = Some(first.map_or(mi, |f| f.min(mi)));
                    sizes = self.sizes_now();
                }
            }
            // `leave` writes the anchor again: the first message changes with it, and with it every topic's
            // pocket, so a squeeze leaves only when no pocket that lives holds the anchor as it is
            if target != 0.0
                && sizes.iter().sum::<i64>() as f64 > target
                && (!squeeze || warm == 0)
                && self.leave(&sizes, target)
            {
                first = Some(0);
            }
            if squeeze {
                self.bump("pocket_squeeze", first.is_some() as i64);
                self.miss("miss_squeeze", first, &sizes, t);
            } else {
                self.bump("cap_rewrites", first.is_some() as i64);
                self.miss("miss_cap", first, &sizes, t);
            }
        } else if due {
            if let Some(e) = self.evict(t, k, &sizes, false) {
                self.miss("miss_due", Some(e), &sizes, t);
                first = Some(first.map_or(e, |f| f.min(e)));
            }
        }
        // what settles and what fits is weighed as a view rewritten from message 1 on (`first`); the cache
        // misses from the oldest result settled, and from the anchor that `leave` writes again
        let mut missed = first;
        if let Some(mi) = self.settle(t) {
            first.get_or_insert(1);
            self.miss("miss_settle", Some(mi), &sizes, t);
            missed = Some(missed.map_or(mi, |f| f.min(mi)));
        }
        if self.fit() {
            first.get_or_insert(1);
            self.miss("miss_fit", Some(0), &sizes, t);
            missed = Some(0);
        }
        if first.is_some() || due {
            self.checked = self.sizes_now().iter().sum();
            self.checked_at = k;
        }
        self.miss("miss_close", closed, &sizes, t);
        if let Some(c) = closed {
            first = Some(first.map_or(c, |f| f.min(c)));
            missed = Some(missed.map_or(c, |f| f.min(c)));
        }
        if let Some(mi) = self.nudge(t, first.is_some()) {
            self.miss("miss_nudge", Some(mi), &sizes, t);
            first = Some(first.map_or(mi, |f| f.min(mi)));
            missed = Some(missed.map_or(mi, |f| f.min(mi)));
        }
        self.recite(t);
        // a view changed from message 1 on is read again nearly whole: the anchor can change with it, as it can
        // when no pocket that lives holds the anchor as it is (`Params.pockets`)
        if self.refresh_rules(first.is_some_and(|f| f <= 1) || warm == 0) {
            self.miss("miss_anchor", Some(0), &sizes, t);
            missed = Some(0);
        }
        for (k, mi) in quiet {
            self.miss(k, Some(mi), &sizes, t);
            missed = Some(missed.map_or(mi, |f| f.min(mi)));
        }
        self.miss("miss", missed, &sizes, t);
        self.sent_vis = self.visible();
        if self.p.pockets != 0 {
            self.keep_pocket();
        }
        self.warm = usize::MAX;
    }

    /// The view `vis` as sent, message by message: (message, the hash of it as it goes), so that two views
    /// hash alike as far as the provider reads them alike.
    fn prints(&mut self, vis: &[usize]) -> Vec<(usize, u64)> {
        if self.raw_fp.len() < self.tr.msgs.len() {
            self.raw_fp.resize(self.tr.msgs.len(), 0);
        }
        vis.iter()
            .map(|&mi| {
                if self.message(mi).0.is_some() {
                    return (mi, self.built[&mi].fp);
                }
                if self.raw_fp[mi] == 0 {
                    self.raw_fp[mi] = fingerprint(&self.tr.msgs[mi]);
                }
                (mi, self.raw_fp[mi])
            })
            .collect()
    }

    /// The topic whose pocket the view is (`Params.pockets`): the one the person is on when the other topics
    /// leave the view, else the one whole view.
    fn pocket_key(&self) -> usize {
        if !self.goal.turns.is_empty() && self.apart() {
            self.goal.active()
        } else {
            usize::MAX
        }
    }

    /// Where the provider's cache stops in the view as it stands, by the pockets (`Params.pockets`): the first
    /// message no pocket that lives holds, the same by every pocket however old, and whether nothing past the
    /// anchor is held (`usize::MAX`: all of it is held, or the cache has seen none of it yet). Counts what a
    /// pocket older than the latest view brings, and a topic come back to after its pocket is gone.
    fn warm_now(&mut self) -> (usize, usize, bool) {
        if self.pockets.is_empty() {
            return (usize::MAX, usize::MAX, false);
        }
        let vis = self.visible();
        let cur = self.prints(&vis);
        let life = self.p.life.min(self.life_hi);
        let young = self.pockets.values().map(|p| p.at).fold(f64::NEG_INFINITY, f64::max);
        let (now, own) = (self.now, self.pocket_key());
        // each pocket: whether it lives, whether it is the latest, whether it is this topic's, and the first
        // place of the view it does not hold (`usize::MAX`: none)
        let held: Vec<(bool, bool, bool, usize)> = self
            .pockets
            .iter()
            .map(|(&k, p)| {
                let n = cur.iter().zip(&p.sent).take_while(|(a, b)| a == b).count();
                let lives = if life > 0.0 { now - p.at <= life } else { p.at >= young };
                (lives, p.at >= young, k == own, if n == p.sent.len().min(cur.len()) { usize::MAX } else { n })
            })
            .collect();
        let live = held.iter().filter(|h| h.0).map(|h| h.3).max();
        let all = held.iter().map(|h| h.3).max().unwrap_or(0);
        let latest = held.iter().filter(|h| h.1).map(|h| h.3).max().unwrap_or(0);
        if let Some(c) = live.filter(|&c| c > latest) {
            let tokens: i64 = (latest..c.min(cur.len())).map(|j| self.message(cur[j].0).1).sum();
            self.bump("pocket_back", 1);
            self.bump("pocket_hit", tokens);
        }
        if held.iter().any(|h| h.2 && !h.0) {
            self.bump("pocket_dead", 1);
        }
        let place = |c: usize| if c == usize::MAX { usize::MAX } else { cur[c].0 };
        let warm = live.map_or(0, place);
        (warm, place(all), warm != usize::MAX && live.is_none_or(|c| c <= 1))
    }

    /// The view just decided is the pocket of its topic (`Params.pockets`); what the cache gives it by how
    /// long the cache keeps views is kept for `learn`.
    fn keep_pocket(&mut self) {
        let vis = self.visible();
        let fin = self.prints(&vis);
        let tokens: Vec<i64> = vis.iter().map(|&mi| self.message(mi).1).collect();
        let now = self.now;
        let mut by_age: Vec<(f64, i64)> = self
            .pockets
            .values()
            .map(|p| {
                let n = fin.iter().zip(&p.sent).take_while(|(a, b)| a == b).count();
                (now - p.at, tokens[..n].iter().sum())
            })
            .collect();
        by_age.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut steps: Vec<(f64, i64)> = Vec::new();
        for (age, hit) in by_age {
            if steps.last().is_none_or(|s| hit > s.1) {
                steps.push((age, hit));
            }
        }
        self.probe = Some(Probe { steps, total: tokens.iter().sum() });
        let key = self.pocket_key();
        self.pockets.insert(key, Pocket { sent: fin, at: now });
        if self.pockets.len() > POCKETS_KEPT {
            let oldest = self.pockets.iter().min_by(|a, b| a.1.at.total_cmp(&b.1.at)).map(|(k, _)| *k);
            if let Some(k) = oldest {
                self.pockets.remove(&k);
            }
        }
    }

    /// What the provider said of the call made with the view last decided (`Curator::cached`): the input
    /// tokens it read from its cache, and all of them. The share it read tells which pockets it still kept,
    /// so how long it keeps a view: a pocket it gave sets the least lifetime, one it did not give the most,
    /// unless that age is too young to tell (`LIFE_LEAST`) or below one it gave. A provider that never says
    /// it read from its cache teaches nothing.
    pub fn learn(&mut self, sys: u64, read: u64, input: u64) {
        let Some(probe) = self.probe.take() else { return };
        self.read_seen |= read > 0;
        if input == 0 || !self.read_seen {
            return;
        }
        let all = (sys as i64 + probe.total).max(1) as f64;
        // what it read, in the estimated tokens of the view
        let got = read.min(input) as f64 / input as f64 * all;
        let mut below = 0.0;
        for &(age, hit) in &probe.steps {
            let level = sys as f64 + hit as f64;
            let gap = level - below;
            if gap >= LIFE_GAP * all {
                if got >= below + 0.7 * gap {
                    self.life_lo = self.life_lo.max(age);
                } else {
                    if got <= below + 0.3 * gap && age >= LIFE_LEAST && age > self.life_lo && age < self.life_hi {
                        self.life_hi = age;
                        self.bump("life_learned", 1);
                    }
                    break;
                }
            }
            below = level;
        }
        // it gave a view older than the lifetime learned: that was the provider's load, not its lifetime
        if self.life_lo >= self.life_hi {
            self.life_hi = f64::INFINITY;
            self.bump("life_reset", 1);
        }
    }

    /// Counts under `k` the tokens of the view from message `from` on: what the provider's cache misses
    /// when that message is the first one the view changed (`None`: no change, nothing counted).
    fn miss(&mut self, k: &'static str, from: Option<usize>, sizes: &[i64], t: usize) {
        if let Some(f) = from {
            let n = sizes.len().min(t);
            let tokens = sizes[f.min(n)..n].iter().sum();
            self.bump(k, tokens);
        }
    }

    /// `Params.recite`: the open request, in the person's words, goes again at the end of the latest output
    /// once the agent has taken `n`, `2n`, `4n`… steps on it without answering it.
    fn recite(&mut self, t: usize) {
        let stall = self.p.stall.max(0) as usize;
        let n = if stall > 0 { stall as i64 } else { self.p.recite };
        if n <= 0 || t < 2 {
            return;
        }
        // the person's latest words, also when they ride on the results of a step (a host that adds the
        // next request to them, a person who writes while the agent works): that is the request now open
        let said = |mi: usize| -> Vec<String> {
            self.fz
                .by_msg(mi)
                .iter()
                .filter(|&&i| self.tr.units[i].kind == UKind::User)
                .map(|&i| self.tr.unit_text(i).into_owned())
                .filter(|s| crate::learn::is_request(s))
                .collect()
        };
        let Some(start) = (0..t).rev().find(|&mi| !said(mi).is_empty()) else { return };
        if start + 1 >= t {
            return;
        }
        if self.recite_next.0 != start {
            self.recite_next = (start, n as usize);
        }
        let last = t - 1;
        let result = blocks(self.tr.msgs[last].get("content"))
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
        // the agent's steps on the request: its messages since the request
        let steps = (start + 1..t)
            .filter(|&mi| self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant"))
            .count();
        if !result || steps < self.recite_next.1 || self.recited.contains_key(&last) {
            return;
        }
        // with `stall`: only once the last outputs brought next to nothing new
        let mut fact = String::new();
        if stall > 0 {
            let mine: Vec<_> = self.novel.iter().filter(|x| x.0 > start && x.0 < t).collect();
            let tail = &mine[mine.len().saturating_sub(stall)..];
            let (new, total) = tail.iter().fold((0, 0), |(a, b), x| (a + x.1, b + x.2));
            if tail.len() < stall || total == 0 || new as f64 >= STALL_NEW * total as f64 {
                return;
            }
            fact = format!(
                "{MARK} Step {steps} on this request: of the {total} lines the last {} outputs gave, {new} were new.\n",
                tail.len()
            );
        }
        let asked = said(start).join("\n");
        if asked.trim().is_empty() {
            return;
        }
        let asked = clipped(asked.trim(), RECITE_CHARS);
        self.recited
            .insert(last, format!("{fact}{MARK} The request this work is for, as the person wrote it:\n{asked}"));
        self.built.remove(&last);
        self.recite_next.1 *= 2;
        self.bump("recited", 1);
    }

    /// The calls and outputs of request `j` (the messages between the request and its reply), if the agent
    /// answered it: its last message holds text and no call. `None` for a request still open or with no
    /// calls.
    fn work_of(&self, j: usize) -> Option<(usize, Vec<usize>)> {
        let (a, b) = self.goal.span(j, self.tr.msgs.len());
        let has = |mi: usize, kind: &str| {
            blocks(self.tr.msgs[mi].get("content")).iter().any(|x| x.get("type").and_then(Value::as_str) == Some(kind))
        };
        let last = b - 1;
        let answered = last > a
            && self.tr.msgs[last].get("role").and_then(Value::as_str) == Some("assistant")
            && !has(last, "tool_use")
            && self.fz.by_msg(last).iter().any(|&i| self.tr.units[i].kind == UKind::Text);
        let work: Vec<usize> = (a + 1..last).filter(|&mi| has(mi, "tool_use") || has(mi, "tool_result")).collect();
        // task=1: a request the person left for a newer one is over too, answered or not
        let left = self.p.task > 0 && j + 1 < self.goal.turns.len();
        ((answered || left) && !work.is_empty()).then_some((a, work))
    }

    /// The distinctive words of the units of `msgs` whose kind is one of `kinds`.
    fn words_in(&self, msgs: impl Iterator<Item = usize>, kinds: &[UKind]) -> FxHashSet<Sym> {
        let mut out = FxHashSet::default();
        for mi in msgs {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if kinds.contains(&u.kind) {
                    out.extend(u.words.iter().copied());
                }
            }
        }
        out
    }

    /// The words of request `j` that point at some steps and not at all: its distinctive words and its
    /// topic words that at most half of the steps hold and that at most half of the requests say. A word
    /// every step holds (the project's folder) or every request repeats (the frame of the session: a
    /// format, a name the person always gives) says nothing of which steps to bring, unless exactly one
    /// step holds it: then it points at that step however often the person says it.
    fn asks(&mut self, j: usize) -> FxHashSet<Sym> {
        while self.requests.len() <= j {
            let a = self.goal.turns[self.requests.len()].start;
            let mut words = self.words_in(std::iter::once(a), &[UKind::User]);
            let text = self.unit_text_at(a, UKind::User);
            words.extend(topics(&text, &mut self.int));
            for &w in &words {
                *self.requests_df.entry(w).or_default() += 1;
            }
            self.requests.push(words);
        }
        let (n, m) = (self.steps.len(), self.requests.len());
        let df = |map: &FxHashMap<Sym, usize>, w: &Sym| map.get(w).copied().unwrap_or(0);
        self.requests[j]
            .iter()
            .filter(|w| {
                df(&self.step_df, w) == 1 || (2 * df(&self.step_df, w) <= n && 2 * df(&self.requests_df, w) <= m + 1)
            })
            .copied()
            .collect()
    }

    /// The files each step read, each by the parts of its name the other files read do not share
    /// (`assembly-B.txt` by `B` next to `assembly-A.txt`; `auth.rs` by `auth` next to `db.rs`; a file read
    /// alone, by all of them). A part two files have (`assembly`, `test`) names neither; a file with no
    /// part of its own keeps them all.
    fn step_files(&mut self) -> Arc<Vec<Vec<Vec<String>>>> {
        for s in &self.steps[self.sf.seen()..] {
            self.sf.push(s.did.iter().flat_map(|d| files_in(d)).collect());
        }
        self.sf.all()
    }

    /// `Params.noted`: a step of an answered request is idle once the agent's notes written after it name
    /// every file it read (`step_files`), and only then; a step that read no file is named by any later
    /// note. Short parts are matched with their case, so a file `A` is not named by the article «a». An
    /// agent that has written no note yet keeps the idle steps `close_unused` found.
    fn noted_close(&mut self, now: usize) {
        let n = self.tr.msgs.len();
        for mi in self.noted_upto..n {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if u.kind != UKind::Text && !(u.kind == UKind::Call && u.tool == NOTE_TOOL) {
                    continue;
                }
                let text = self.tr.unit_text(i);
                for c in NOTE_LINE.captures_all(&text) {
                    let line = c[2].map_or("", |(x, y)| &text[x..y]);
                    self.noted.push((mi, name_parts(line).into_iter().collect()));
                }
            }
        }
        self.noted_upto = n;
        // an agent that writes no notes is not judged by them: its steps go idle as `close_unused` says
        if self.noted.is_empty() {
            return;
        }
        let files = self.step_files();
        let latest = self.goal.turns.last().map_or(0, |t| t.start);
        for (i, s) in self.steps.iter_mut().enumerate().filter(|(_, s)| !s.out) {
            let after: Vec<&FxHashSet<String>> =
                self.noted.iter().filter(|(mi, _)| *mi > s.msgs[0]).map(|(_, w)| w).collect();
            let named = |own: &Vec<String>| own.iter().all(|p| after.iter().any(|w| w.contains(p.as_str())));
            let done = !after.is_empty() && files[i].iter().all(named);
            s.idle = if s.a < latest && done { Some(s.idle.unwrap_or(now)) } else { None };
        }
    }

    /// The outputs of the calls that read a file the latest request names, each file by the parts of its
    /// name the other files read do not share (as in `step_files`): «and now Function C» names
    /// `assembly-C.txt` next to `assembly-A.txt`. It looks at every call, the request still open too.
    /// Newest first, as `named_take` bounds them.
    fn named_results(&self) -> FxHashSet<usize> {
        let mut out = FxHashSet::default();
        let Some(start) = self.goal.turns.last().map(|t| t.start) else { return out };
        let asked: FxHashSet<String> = name_parts(&self.unit_text_at(start, UKind::User)).into_iter().collect();
        let cands: Vec<(Vec<usize>, bool)> = self
            .call_files()
            .into_iter()
            .rev()
            .filter(|(_, files)| files.iter().any(|f| f.iter().all(|p| asked.contains(p))))
            .map(|(mi, _)| {
                let res = self.fz.by_msg(mi + 1).iter().copied().filter(|&u| self.tr.units[u].kind == UKind::Result);
                (res.collect(), true)
            })
            .collect();
        for ((units, _), take) in cands.iter().zip(self.named_take(&cands)) {
            if take {
                out.extend(units.iter().copied());
            }
        }
        out
    }

    /// The files result `i` holds as they were: a read tool's file, or those a shell command that only
    /// printed files printed (`shell_reads`).
    fn read_paths(&self, i: usize) -> Vec<&str> {
        match self.fz.info.get(&i) {
            Some(x) if x.kind == Kind::Read => x.path.as_deref().filter(|p| !p.is_empty()).into_iter().collect(),
            _ => self.shell_reads.get(&i).map(|ps| ps.iter().map(String::as_str).collect()).unwrap_or_default(),
        }
    }

    /// Of the reads a request names (`cands`: the result units of each, newest first), the ones `named`
    /// keeps or brings back, newest first up to `Params.named` tokens: none a later edit of its file made
    /// out of date once a newer read of that file is taken (it shows what the file no longer holds; with
    /// no newer read, it is still the best picture of the file, the edits on top), and none whose words a
    /// newer one taken holds already (a second read of the same lines). The newest one still in the view
    /// stays whatever its size (`in_view`): keeping it adds nothing to send. Whether each is taken.
    fn named_take(&self, cands: &[(Vec<usize>, bool)]) -> Vec<bool> {
        let budget = self.p.named;
        let (mut spent, mut covered, mut fresh) = (0i64, FxHashSet::default(), Vec::<&str>::new());
        let mut out = Vec::with_capacity(cands.len());
        for (units, in_view) in cands {
            let paths: Vec<(usize, Vec<&str>)> =
                units.iter().map(|&u| (u, self.read_paths(u))).filter(|(_, ps)| !ps.is_empty()).collect();
            let newer = |p: &&str| fresh.iter().any(|f| same_file(f, p));
            let stale = !paths.is_empty()
                && paths.iter().all(|(u, ps)| ps.iter().all(newer) && self.read_superseded(*u, true).is_some());
            let words: FxHashSet<Sym> = units.iter().flat_map(|&u| self.tr.units[u].words.iter().copied()).collect();
            let copy = !words.is_empty() && words.iter().all(|w| covered.contains(w));
            let tokens: i64 = units.iter().map(|&u| self.tr.units[u].tokens).sum();
            let fits = spent + tokens <= budget || (*in_view && spent == 0);
            let take = !stale && !copy && fits;
            if take {
                spent += tokens;
                covered.extend(words);
                fresh.extend(paths.into_iter().flat_map(|(_, ps)| ps));
            }
            out.push(take);
        }
        out
    }

    /// The calls that read files, each with its message and its files, each file by the parts of its name
    /// the files the other calls read do not share (as in `step_files`); a file with no part of its own
    /// keeps them all.
    fn call_files(&self) -> Vec<(usize, Vec<Vec<String>>)> {
        let shared = |p: &String| self.part_files.get(p).copied().unwrap_or(0) > 1;
        let own = |f: &Vec<String>| {
            let own: Vec<String> = f.iter().filter(|p| !shared(p)).cloned().collect();
            if own.is_empty() {
                f.clone()
            } else {
                own
            }
        };
        self.calls_raw.iter().map(|(mi, fs)| (*mi, fs.iter().map(own).collect())).collect()
    }

    /// Brings `calls_raw` and the part counts up to the units that came since the last request.
    fn index_calls(&mut self) {
        for u in self.calls_seen..self.tr.units.len() {
            if self.tr.units[u].kind != UKind::Call || self.tr.units[u].tool == NOTE_TOOL {
                continue;
            }
            let files = files_in(&action(&self.tr, u));
            if files.is_empty() {
                continue;
            }
            for f in &files {
                if self.files_known.insert(f.clone()) {
                    let mut parts = f.clone();
                    parts.sort();
                    parts.dedup();
                    for p in parts {
                        *self.part_files.entry(p).or_default() += 1;
                    }
                }
            }
            self.calls_raw.push((self.tr.units[u].msg, files));
        }
        self.calls_seen = self.tr.units.len();
    }

    /// With `noted`, the outputs of the calls whose files the agent's notes written after them all name,
    /// once the notes have shown they are enough (`note_holds`): what the agent wrote down stands in for
    /// what it read, so the output is neither kept for a request that names its file nor held in use by
    /// the words the notes took from it. A call that read no file is not covered: nothing tells which note
    /// is about it.
    fn noted_results(&self) -> FxHashSet<usize> {
        let mut out = FxHashSet::default();
        if self.p.noted == 0 {
            return out;
        }
        for (mi, files) in self.call_files() {
            // the message where the notes written after the call have named every part of its files
            let mut need: FxHashSet<&str> = files.iter().flatten().map(String::as_str).collect();
            let mut at = None;
            for (m, w) in self.noted.iter().filter(|(m, _)| *m > mi) {
                need.retain(|p| !w.contains(*p));
                if need.is_empty() {
                    at = Some(*m);
                    break;
                }
            }
            if let Some(at) = at {
                if files.iter().all(|f| self.note_holds(at, f)) {
                    out.extend(self.fz.by_msg(mi + 1).iter().copied());
                }
            }
        }
        out
    }

    /// Whether the notes written by message `from` were enough for file `f` (by the parts of its name the
    /// other files do not share): a later request that names the file got its reply with no call that read
    /// the file again. A note can hold only part of what was read (the path it took, not the one the next
    /// request asks for); the agent going back to the file says so, and the read stays.
    fn note_holds(&self, from: usize, f: &[String]) -> bool {
        let starts: FxHashSet<usize> = self.goal.turns.iter().map(|t| t.start).collect();
        let reads = |u: usize| files_in(&action(&self.tr, u)).iter().any(|g| f.iter().all(|p| g.contains(p)));
        for t in self.goal.turns.iter().filter(|t| t.start > from) {
            let asked: FxHashSet<String> = name_parts(&self.unit_text_at(t.start, UKind::User)).into_iter().collect();
            if !f.iter().all(|p| asked.contains(p)) {
                continue;
            }
            for mi in t.start + 1..self.tr.msgs.len() {
                if starts.contains(&mi) {
                    break;
                }
                let units = self.fz.by_msg(mi).iter().map(|&u| (u, &self.tr.units[u]));
                let calls: Vec<usize> = units
                    .clone()
                    .filter(|(_, x)| x.kind == UKind::Call && x.tool != NOTE_TOOL)
                    .map(|(u, _)| u)
                    .collect();
                if calls.into_iter().any(reads) {
                    break;
                }
                if units.into_iter().any(|(_, x)| x.kind == UKind::Text) {
                    return true;
                }
            }
        }
        false
    }

    /// `Params.named`: a step that read a file the latest request names (`step_files`) is not idle. «Fix
    /// the login in auth» keeps `auth.rs` read; the next request, on the README, lets it go. When a request
    /// has just begun (`turned`), the steps out of the view that read a file it names come back, the way
    /// `recall` brings them: «and now the logout in auth» finds `auth.rs` read again. Whether any came back.
    fn named_keep(&mut self, turned: bool) -> bool {
        let Some(start) = self.goal.turns.last().map(|t| t.start) else { return false };
        let asked: FxHashSet<String> = name_parts(&self.unit_text_at(start, UKind::User)).into_iter().collect();
        let files = self.step_files();
        let last = self.tr.msgs.len() - 1;
        // `mission`: the files the thread's first message names that no later request of it named yet
        let thread = self.goal.active();
        let parts: Vec<FxHashSet<String>> = if self.p.mission != 0 {
            let turns: Vec<usize> = self.goal.turns.iter().filter(|t| t.thread == thread).map(|t| t.start).collect();
            turns.iter().map(|&s| name_parts(&self.unit_text_at(s, UKind::User)).into_iter().collect()).collect()
        } else {
            Vec::new()
        };
        let listed = |f: &Vec<String>| parts.len() > 1 && f.iter().all(|p| parts[0].contains(p));
        let taken = |f: &Vec<String>| parts[1..].iter().any(|r| f.iter().all(|p| r.contains(p)));
        // only a list being worked through: a later request already took up one of the files it names
        let through = files.iter().flatten().any(|f| listed(f) && taken(f));
        let pending = |f: &Vec<String>| through && listed(f) && !taken(f);
        let mut back = 0;
        let mut named: Vec<(usize, Vec<usize>, bool)> = Vec::new();
        let noted = self.noted_results();
        for (i, own) in files.iter().enumerate() {
            // what the agent wrote down about the file, once it proved enough, stands in for its reads:
            // they are not kept for the request that names it, nor brought back (`noted`)
            let outs = self.steps[i]
                .msgs
                .iter()
                .flat_map(|&m| self.fz.by_msg(m))
                .filter(|&&u| self.tr.units[u].kind == UKind::Result);
            if outs.clone().next().is_some() && outs.into_iter().all(|u| noted.contains(u)) {
                continue;
            }
            if !own.iter().any(|own| own.iter().all(|p| asked.contains(p))) {
                if !self.steps[i].out && own.iter().any(pending) {
                    self.steps[i].idle = None;
                }
                continue;
            }
            // an output that left unused (`live_drop`) is gone from the view as much as a step that left
            let res: Vec<usize> = self.steps[i]
                .msgs
                .iter()
                .flat_map(|&mi| self.fz.by_msg(mi).iter().copied())
                .filter(|&u| self.tr.units[u].kind == UKind::Result)
                .collect();
            let gone = self.steps[i].at.is_none() && !res.is_empty() && res.iter().all(|u| self.led.contains(u));
            named.push((i, res, !self.steps[i].out && !gone));
        }
        // reads by parts (`sed -n 40,400p`, `400,800p`) are not copies of each other: each may stay or come
        // back, newest first, within the budget (`named_take`)
        named.reverse();
        let cands: Vec<(Vec<usize>, bool)> = named.iter().map(|(_, res, in_view)| (res.clone(), *in_view)).collect();
        for ((i, _, in_view), take) in named.into_iter().zip(self.named_take(&cands)) {
            if !take {
                continue;
            }
            if in_view {
                self.steps[i].idle = None;
            } else if turned {
                let text = self.step_text(i);
                let s = &mut self.steps[i];
                (s.out, s.at, s.tokens, s.idle, s.back) =
                    (false, Some(last), est_str(&text), None, s.back.saturating_add(1));
                back += 1;
            }
        }
        if back > 0 {
            self.bump("recalled_steps", back);
        }
        back > 0
    }

    /// A request has just ended (`k`, the one before the latest): each step of the answered requests
    /// before it (a call and its output) that `k` drew nothing from leaves the view. Drawing on a step is
    /// writing, in a call or in text, a distinctive word of its output that neither the person's requests
    /// nor `k`'s own outputs hold: the agent can only have taken it from that step, or from a reply that
    /// did. While the requests that follow keep using a step, it stays (a second question on the same file
    /// finds the file read); the first request done without it may close it (`rent_close`). A step the
    /// latest request names (`need`, as `pick` takes them) stays too. `k`'s own steps wait for the
    /// request after it. The steps that may leave.
    fn close_unused(&mut self, k: usize, j: usize) -> Option<FxHashSet<usize>> {
        let n = self.tr.msgs.len();
        let (ka, kb) = self.goal.span(k, n);
        let said = self.words_in(ka..kb, &[UKind::Text, UKind::Call]);
        let own = self.words_in(ka..kb, &[UKind::Result]);
        let asked = self.words_in(std::iter::once(0).chain(self.goal.turns.iter().map(|t| t.start)), &[UKind::User]);
        let said: FxHashSet<Sym> = said.into_iter().filter(|w| !asked.contains(w)).collect();
        // Only a word of some output shows where the request took what it knew: a request that read
        // nothing and used no output (a bare "[1, 2]", or words of the earlier replies) lets no step go.
        let told = !own.is_empty() || self.steps.iter().any(|s| !s.out && s.read.iter().any(|w| said.contains(w)));
        // close=2: every step leaves once its request is answered, used or not
        let (told, said) = if self.p.close >= 2 { (true, FxHashSet::default()) } else { (told, said) };
        let said: FxHashSet<Sym> = said.into_iter().filter(|w| !own.contains(w)).collect();
        let mut unused: FxHashSet<usize> = FxHashSet::default();
        if told {
            for (i, s) in self.steps.iter().enumerate() {
                if !s.out && !s.read.iter().any(|w| said.contains(w)) {
                    unused.insert(i);
                }
            }
        }
        if let Some((a, work)) = self.work_of(k) {
            for msgs in steps_of(&self.tr, work) {
                let s = self.new_step(a, msgs);
                if self.p.close >= 2 {
                    unused.insert(self.steps.len());
                }
                for &m in &s.msgs {
                    self.step_at.insert(m, self.steps.len());
                }
                self.steps.push(s);
            }
        }
        // what the latest request names stays: the steps recall would bring back for it
        let need = self.asks(j);
        for i in self.pick(&need, FxHashSet::default(), |s| !s.out) {
            unused.remove(&i);
        }
        told.then_some(unused)
    }

    /// A step of request message `a`, made of messages `msgs`: what it read, what it is about, what it did.
    fn new_step(&mut self, a: usize, msgs: Vec<usize>) -> Step {
        let mut called = self.words_in(msgs.iter().copied(), &[UKind::Call]);
        // a request names a file by its name more often than by its path
        let paths: Vec<String> = called.iter().map(|&w| self.int.get(w).to_string()).collect();
        for p in paths {
            match p.rsplit_once('/') {
                Some((_, name)) if name.chars().count() >= 4 => {
                    called.insert(self.int.intern(name));
                }
                _ => {}
            }
        }
        let read = self.words_in(msgs.iter().copied(), &[UKind::Result]);
        let did = self.fz.by_msg(msgs[0]).iter().filter(|&&i| self.tr.units[i].kind == UKind::Call);
        let did: Vec<String> = did.map(|&i| action(&self.tr, i)).collect();
        let mut about: FxHashSet<Sym> = called.union(&read).copied().collect();
        for mi in &msgs {
            let text = self.unit_text_at(*mi, UKind::Result);
            about.extend(topics(&text, &mut self.int));
        }
        let mut aimed = called;
        for d in &did {
            aimed.extend(topics(d, &mut self.int));
        }
        about.extend(aimed.iter().copied());
        for &w in &about {
            *self.step_df.entry(w).or_default() += 1;
        }
        // what the agent brought back of it while its request was open counts as the step brought back
        let back = msgs.iter().filter_map(|m| self.brought.remove(m)).fold(0u32, u32::saturating_add);
        Step { a, msgs, read, about, aimed, did, out: false, at: None, tokens: 0, idle: None, back }
    }

    /// The outputs of the request still open (`Params.live`), all but those after the agent's latest
    /// message: one that no agent message written after it drew on (a distinctive word of it that the
    /// person's requests do not hold, in text or in a call) goes idle; one drawn on is in use. The idle
    /// ones that pay for leaving now (the rent rule of `rent_close`) leave whole, each in its place, the
    /// call that asked for it kept. The agent works from what it reads: what it never took up in the calls
    /// and text that followed was not what it needed, and if it is, it can run the call again. The oldest
    /// message an output left from, if any left.
    fn live_drop(&mut self, now: usize) -> Option<usize> {
        let (n, nt) = (self.tr.msgs.len(), self.goal.turns.len());
        // with `named`, the outputs of a file the latest request names stay: the request is about that file
        let mut kept = if self.p.named != 0 { self.named_results() } else { FxHashSet::default() };
        // the outputs the agent's notes already cover are not kept for it, nor in use (`noted`)
        let noted = self.noted_results();
        kept.retain(|u| !noted.contains(u));
        let (a, _) = self.goal.span(nt - 1, n);
        let asked = self.words_in(std::iter::once(0).chain(self.goal.turns.iter().map(|t| t.start)), &[UKind::User]);
        let (mut said, mut wrote, mut told) = (FxHashSet::default(), 0i64, false);
        let mut idle: Vec<(usize, usize, i64)> = Vec::new();
        for mi in (a + 1..n).rev() {
            if self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant") {
                wrote += 1;
            }
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                match u.kind {
                    UKind::Text | UKind::Call => {
                        told |= u.kind == UKind::Text || u.tool == NOTE_TOOL;
                        // with `follow`, a name a call went for that the call's own output holds was followed
                        // there: it does not keep the older output that pointed to it in use
                        let got: FxHashSet<Sym> = if self.p.follow != 0 && u.kind == UKind::Call {
                            let res = self.fz.by_msg(mi + 1).iter().map(|&k| &self.tr.units[k]);
                            res.filter(|r| r.kind == UKind::Result).flat_map(|r| r.words.iter().copied()).collect()
                        } else {
                            FxHashSet::default()
                        };
                        said.extend(u.words.iter().copied().filter(|w| !asked.contains(w) && !got.contains(w)));
                    }
                    UKind::Result
                        if wrote >= self.p.live && u.tokens >= SMALL_RESULT && (told || self.p.digest == 0) =>
                    {
                        let gone = self.led.contains(&i)
                            || self.dropped.get(&i).map_or(0, |d| d.len()) == self.fz.chunks[i].len();
                        // an output that sends the lines of a copy left out stays: its stub says this one
                        // sends them, and with both gone they are lost
                        let carries = (0..self.fz.chunks[i].len()).any(|j| self.carrying.contains(&(i, j)));
                        if gone || carries || kept.contains(&i) {
                        } else if !noted.contains(&i) && u.words.iter().any(|w| said.contains(w)) {
                            self.live_since.remove(&i);
                        } else {
                            idle.push((mi, i, u.tokens));
                        }
                    }
                    _ => {}
                }
            }
        }
        if idle.is_empty() {
            return None;
        }
        idle.reverse();
        // an output is priced at what it sends now: its chunks not dropped yet, less the lines that copies
        // the view keeps point to (were it to leave, those copies would send them)
        let going: FxHashSet<usize> = idle.iter().map(|&(mi, _, _)| mi).collect();
        let pointed = self.pointed(|m| !going.contains(&m));
        for e in idle.iter_mut() {
            e.2 = self.sends_now(e.1, pointed.get(&e.1));
        }
        let sizes = self.sizes_now();
        let mut after = vec![0i64; sizes.len() + 1];
        for mi in (0..sizes.len()).rev() {
            after[mi] = after[mi + 1] + sizes[mi];
        }
        // the cut is priced on the whole view behind it, the messages the previous request did not cache
        // too. Priced only on what it cached, as `rent` does, was measured on the 20 recorded sessions:
        // 1.2 % more input cost and 2 more forgotten words of 112.
        let r = self.p.ratio;
        let (mut best, mut cut, mut paid, mut saved) = (0.0f64, None, 0.0f64, 0i64);
        for k in (0..idle.len()).rev() {
            let (mi, i, size) = idle[k];
            let since = now.saturating_sub(*self.live_since.entry(i).or_insert(now));
            paid += r * size as f64 * (since as f64 + self.p.ahead * now as f64);
            saved += size;
            let gain = paid - self.p.margin * (1.0 - r) * (after[mi] - saved).max(0) as f64;
            if gain > best {
                (best, cut) = (gain, Some(k));
            }
        }
        let cut = cut?;
        for &(_, i, _) in &idle[cut..] {
            let all = 0..self.fz.chunks[i].len();
            if self.p.lead != 0 {
                let lines = self.leads(i);
                if !lines.is_empty() {
                    self.lead_lines.insert(i, lines);
                }
            }
            self.drop_as(i, all, "live");
            self.led.insert(i);
            self.live_since.remove(&i);
            self.bump("live_dropped", 1);
        }
        self.vis = None;
        // the outputs are in the order of their messages
        Some(idle[cut].0)
    }

    /// The tokens output `i` sends now: its chunks not dropped yet, less the share of its lines that
    /// copies kept in the view point to (`pointed`, from `Engine::pointed`).
    fn sends_now(&self, i: usize, pointed: Option<&Vec<bool>>) -> i64 {
        let (chunks, have) = (&self.fz.chunks[i], self.dropped.get(&i));
        let left: i64 =
            (0..chunks.len()).filter(|j| !have.is_some_and(|h| h.contains(j))).map(|j| chunks[j].tokens).sum();
        let held = pointed
            .map_or(0, |p| self.tr.units[i].tokens * p.iter().filter(|x| **x).count() as i64 / (p.len() as i64).max(1));
        (left - held).max(0)
    }

    /// The lines of output `i` that hold a lead: a name no other output holds (nothing printed it again,
    /// so nothing followed it yet), of a form the agent has written in its own text or calls.
    fn leads(&mut self, i: usize) -> Vec<String> {
        for k in self.lead_ix.seen..self.tr.units.len() {
            let u = &self.tr.units[k];
            match u.kind {
                UKind::Text | UKind::Call => {
                    self.lead_ix.forms.extend(u.words.iter().filter_map(|&w| form(self.int.get(w))))
                }
                UKind::Result => {
                    for &w in u.words.iter() {
                        *self.lead_ix.results.entry(w).or_default() += 1;
                    }
                }
                _ => {}
            }
        }
        self.lead_ix.seen = self.tr.units.len();
        let (forms, results) = (&self.lead_ix.forms, &self.lead_ix.results);
        let mine = &self.tr.units[i].words;
        let elsewhere_has = |w: &Sym| results.get(w).copied().unwrap_or(0) > u32::from(mine.contains(w));
        let text = self.tr.unit_text(i).into_owned();
        let mut out = Vec::new();
        for line in text.split('\n') {
            let ws = crate::hindsight::words(line, &mut self.int);
            let lead = |w: &Sym| !elsewhere_has(w) && form(self.int.get(*w)).is_some_and(|f| forms.contains(&f));
            if ws.iter().any(lead) {
                out.push(clipped(line, 200));
            }
        }
        out
    }

    /// The steps of the pool that cover the words of `need` not in `covered`: for each, the richest step
    /// that holds it (a file read whole, more than an edit that names it) and the latest (what changed).
    /// A word no holder went for or read as its own is only a topic many outputs touch (a header every
    /// file repeats, a word of the prose): its richest holder is then the one that says it most often,
    /// as a search ranks by term frequency, not the largest output that names it once.
    /// A step taken covers its other words too, so a request about one listing takes that listing once.
    fn pick(&self, need: &FxHashSet<Sym>, mut covered: FxHashSet<Sym>, pool: impl Fn(&Step) -> bool) -> Vec<usize> {
        let mut want: Vec<Sym> = need.iter().filter(|w| !covered.contains(w)).copied().collect();
        // with `aim`, the words fewest steps hold come first: the one step that has a rare word is the one
        // the request means, and what it covers needs nothing more
        let holders = |w: &Sym| self.steps.iter().filter(|s| pool(s) && s.about.contains(w)).count();
        want.sort_unstable_by_key(|w| (if self.p.aim != 0 { holders(w) } else { 0 }, *w));
        let mut out = Vec::new();
        for w in want {
            if covered.contains(&w) {
                continue;
            }
            let mut holds: Vec<usize> =
                (0..self.steps.len()).filter(|&i| pool(&self.steps[i]) && self.steps[i].about.contains(&w)).collect();
            if self.p.aim != 0 && holds.iter().any(|&i| self.steps[i].aimed.contains(&w)) {
                holds.retain(|&i| self.steps[i].aimed.contains(&w));
            }
            let rich = |i: usize| (self.steps[i].read.len(), self.steps[i].about.len(), self.steps[i].msgs[0]);
            let topic = !holds.iter().any(|&i| self.steps[i].aimed.contains(&w) || self.steps[i].read.contains(&w));
            let richest = if topic {
                let word = self.int.get(w).to_lowercase();
                holds.iter().copied().max_by_key(|&i| (says(&self.step_text(i).to_lowercase(), &word), rich(i)))
            } else {
                holds.iter().copied().max_by_key(|&i| rich(i))
            };
            let latest = holds.iter().copied().max_by_key(|&i| self.steps[i].msgs[0]);
            for i in richest.into_iter().chain(latest) {
                if !out.contains(&i) {
                    covered.extend(self.steps[i].about.iter().copied());
                    out.push(i);
                }
            }
        }
        out
    }

    /// The tokens step `i` takes in the view now (`sizes`: those of each message).
    fn step_size(&self, i: usize, sizes: &[i64]) -> i64 {
        let s = &self.steps[i];
        match s.at {
            Some(_) => s.tokens,
            None => s.msgs.iter().map(|&mi| sizes.get(mi).copied().unwrap_or(0)).sum(),
        }
    }

    /// The idle steps that pay for leaving now leave, in one batch: whatever goes out, the view after the
    /// first of them is read again without the cache once. That costs `(1 - ratio)` of the tokens behind
    /// the cut that stay. Keeping a step costs `ratio` of its tokens on each call; what it has cost since
    /// it went idle is known, and of what is to come it counts `ahead` times the calls so far (0: none,
    /// the rent-or-buy rule, never above twice the best cost; 1: as many as so far). The price of the cut
    /// grows as the view grows behind it, so waiting too long can cost more than leaving early. The cut is
    /// the one with the largest gain, if any gains: the steps before it wait. Whether any step left.
    fn rent_close(&mut self, idle: Vec<usize>) -> bool {
        let sizes = self.sizes_now();
        let now = self.tr.steps.len();
        let r = self.p.ratio;
        let going: FxHashSet<usize> = idle.iter().flat_map(|&i| self.steps[i].msgs.iter().copied()).collect();
        let moved = self.moved(|m| !going.contains(&m));
        let mut cands: Vec<(usize, f64, i64)> = idle
            .into_iter()
            .map(|i| {
                let back: i64 = self.steps[i].msgs.iter().map(|m| moved.get(m).copied().unwrap_or(0)).sum();
                let size = (self.step_size(i, &sizes) - back).max(0);
                let since = now.saturating_sub(self.steps[i].idle.unwrap_or(now));
                let s = &self.steps[i];
                let paid = if self.p.sticky != 0 && s.back > 0 {
                    let p = s.back as f64 / (s.back as f64 + 1.0);
                    r * size as f64 * since as f64 - p * size as f64
                } else {
                    r * size as f64 * (since as f64 + self.p.ahead * now as f64)
                };
                (i, paid, size)
            })
            .collect();
        // a step brought back whose rent since it went idle does not yet pay for bringing it back once more
        // (`sticky`) stays whatever leaves around it: leaving it gains less than nothing, whether or not the
        // view behind it is read again anyway. The rent of any other step is never below zero.
        cands.retain(|c| c.1 >= 0.0);
        cands.sort_by_key(|&(i, _, _)| self.steps[i].place());
        let mut after = vec![0i64; sizes.len() + 1];
        for mi in (0..sizes.len()).rev() {
            after[mi] = after[mi + 1] + sizes[mi];
        }
        // the cut is priced as if the view were read again without the cache from the request the steps were
        // made for: their line goes on the message before them that stays (`line_at`), never ahead of the
        // request, so the price is never below what the cut costs. Priced from that message instead, more
        // leaves sooner and more of what the replies use is lost (fixtures: 127 words forgotten, not 120).
        // The price also counts the messages after the previous request, which no cut makes dearer: priced
        // without them, as `rent` does, input cost falls 3.0 % on the 20 recorded sessions but 2 more of
        // 112 forgotten words are lost, so they stay as a margin.
        // what the cache no longer holds (`Params.pockets` 2) is read again closed or not: closing there costs
        // nothing, and what it saves there makes no closing before it cheaper
        let warm = if self.p.pockets >= 2 { self.warm } else { usize::MAX };
        let (mut best, mut cut, mut paid, mut saved, mut first) = (0.0f64, None, 0.0f64, 0i64, sizes.len());
        for n in (0..cands.len()).rev() {
            paid += cands[n].1;
            let s = &self.steps[cands[n].0];
            if s.place() < warm {
                saved += cands[n].2;
            }
            first = first.min(s.place()).min(s.a);
            let rest = (after[first] - after[first.max(warm).min(sizes.len())] - saved).max(0);
            let gain = paid - self.p.margin * (1.0 - r) * rest as f64;
            if gain > best {
                (best, cut) = (gain, Some(n));
            }
        }
        let Some(cut) = cut else { return false };
        for &(i, _, _) in &cands[cut..] {
            (self.steps[i].out, self.steps[i].at, self.steps[i].idle) = (true, None, None);
        }
        true
    }

    /// The steps out of the view that the latest request needs come back (`pick`): for each word of `need`
    /// that no output in the view holds, the richest and the latest step out of the view that hold it.
    /// They come back at the end of the view, as text in the latest message, so that what the cache holds
    /// stays as it is; they leave again the way they left the first time. Whether any came back.
    fn recall(&mut self, need: &FxHashSet<Sym>) -> bool {
        let last = self.tr.msgs.len() - 1;
        let vis = self.visible();
        let mut covered = self.words_in(vis.iter().copied(), &[UKind::Result]);
        // only a closed step comes back. An open one the view does not send (it left with the history, or its
        // topic is left out) counts as seen: the ledger keeps the facts of the first, the topic of the second
        // comes back whole when the talk returns to it. Bringing them back here was measured on the 20 recorded
        // sessions: 1.9 % more input cost (up to 29 % in one), and 5 fewer forgotten words of 112 (7 fewer in
        // one, one more in two others).
        for s in self.steps.iter().filter(|s| !s.out) {
            covered.extend(s.about.iter().copied());
        }
        let back = self.pick(need, covered, |s| s.out);
        for &i in &back {
            let text = self.step_text(i);
            let s = &mut self.steps[i];
            (s.out, s.at, s.tokens, s.idle, s.back) =
                (false, Some(last), est_str(&text), None, s.back.saturating_add(1));
        }
        if !back.is_empty() {
            self.bump("recalled_steps", back.len() as i64);
        }
        !back.is_empty()
    }

    /// The calls that just arrived (units `new`) that ask again for what the view last sent had left out:
    /// a `recall` of a message the view did not send, or sent with an output taken out whole; a read of a
    /// file over lines an earlier read gave, or a command that prints files run again, when the latest such
    /// output that came back without an error is out of the view and no edit of the file came after it.
    /// Each is the agent bringing an output back by itself (`wanted_back`). Only the reply to the view last
    /// sent is weighed: a message that comes with it further on (a history taken in at once) was not
    /// written from that view.
    fn asked_back(&mut self, new: std::ops::Range<usize>) {
        for i in new {
            let u = &self.tr.units[i];
            if u.kind != UKind::Call {
                continue;
            }
            let judged = u.msg > 0 && self.sent_vis.last() == Some(&(u.msg - 1));
            let (msg, tool, b) = (u.msg, u.tool.clone(), self.tr.block(i));
            if tool == RECALL_TOOL {
                let name = b.get("input").and_then(|x| x.get("message")).and_then(Value::as_str).unwrap_or("");
                if let Some(mi) = message_named(name).filter(|&mi| judged && mi < msg && self.left_out(mi, None)) {
                    self.wanted_back(mi);
                }
                continue;
            }
            let Some(info) = self.fz.info.get(&i) else { continue };
            let (key, lines, path) = match (info.kind, &info.path) {
                (Kind::Read, Some(p)) => (file_key(p), read_range(b.get("input")), Some(p.clone())),
                (Kind::Exec, _) => {
                    let c = py::squash_ws(&command_of(b));
                    if c.is_empty() || !prints_files(&c) {
                        continue;
                    }
                    (format!("$ {c}"), (1, u64::MAX), None)
                }
                _ => continue,
            };
            if judged {
                // the latest earlier call for some of those lines whose output came without an error
                let copy = self
                    .reads_made
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .rev()
                    .take(ASKED_CALLS)
                    .filter(|&&(_, (a, b))| a <= lines.1 && lines.0 <= b)
                    .find_map(|&(c, _)| self.result_of(c).filter(|&o| !self.fz.info.get(&o).is_some_and(|x| x.error)));
                if let Some(o) = copy {
                    let mi = self.tr.units[o].msg;
                    if self.left_out(mi, Some(o)) && !self.changed_since(mi, &key, path.as_deref()) {
                        self.wanted_back(mi);
                    }
                }
            }
            self.reads_made.entry(key).or_default().push((i, lines));
        }
    }

    /// The output of call unit `c`: the tool result that answers it in the messages after it, if it came.
    fn result_of(&self, c: usize) -> Option<usize> {
        let id = key_of(self.tr.block(c).get("id"));
        let mi = self.tr.units[c].msg;
        (mi + 1..=mi + 3)
            .flat_map(|m| self.fz.by_msg(m).iter().copied())
            .find(|&r| self.tr.units[r].kind == UKind::Result && key_of(self.tr.block(r).get("tool_use_id")) == id)
    }

    /// Whether the view last sent left message `mi` out: it did not send it (a step brought back as text
    /// is sent, in the message it came back at), or an output of it went whole: output `i`, or any of them
    /// when none is given.
    fn left_out(&self, mi: usize, i: Option<usize>) -> bool {
        if self.step_at.get(&mi).is_some_and(|&k| !self.steps[k].out && self.steps[k].at.is_some()) {
            return false;
        }
        if self.sent_vis.binary_search(&mi).is_err() {
            return true;
        }
        let gone = |i: usize| self.dropped.get(&i).is_some_and(|d| d.len() == self.fz.chunks[i].len());
        match i {
            Some(i) => gone(i),
            None => self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result && gone(i)),
        }
    }

    /// Whether an edit or a write that did not fail came after message `since` to a file `call` may have
    /// printed: `path` when the call read one file, else a file the command names (its name is in the
    /// command), or any file when it names none (`ls`, `grep x src/`). As `Params.stale` keeps the edits.
    fn changed_since(&self, since: usize, call: &str, path: Option<&str>) -> bool {
        let any = path.is_none() && files_in(call).is_empty();
        let names = |p: &str| match path {
            Some(f) => same_file(p, f),
            None => any || call.contains(basename(p)),
        };
        let mut after = self.edits.iter().rev().take_while(|e| e.1 > since);
        after.by_ref().take(ASKED_EDITS).any(|(p, _, id)| self.edit_ok.get(id) == Some(&true) && names(p))
            || after.next().is_some()
    }

    /// The agent brought message `mi` back by itself while the view had left it out (`asked_back`): the
    /// step that holds it was needed after it went, as one the curator brings back is, so it counts as
    /// brought back once more (`Step::back`, which `Params.sticky` weighs). While no step holds it yet
    /// (its request is still open), the step made of it starts with the count.
    fn wanted_back(&mut self, mi: usize) {
        match self.step_at.get(&mi).copied() {
            Some(k) => self.steps[k].back = self.steps[k].back.saturating_add(1),
            None => {
                let n = self.brought.entry(mi).or_insert(0);
                *n = n.saturating_add(1);
            }
        }
        self.bump("agent_recalled_steps", 1);
    }

    /// Step `i` as text: what each call did, and each output whole.
    fn step_text(&self, i: usize) -> String {
        let mut out = vec![format!("{MARK} Brought back from earlier in this conversation:")];
        for &mi in &self.steps[i].msgs {
            for &u in self.fz.by_msg(mi) {
                match self.tr.units[u].kind {
                    UKind::Call => out.push(format!("$ {}", action(&self.tr, u))),
                    UKind::Result => out.push(self.tr.unit_text(u).into_owned()),
                    _ => {}
                }
            }
        }
        out.join("\n")
    }

    /// The message the line that says step `i` left goes on (`gone`: the messages out of the view): the
    /// last one before the step that the view still sends (a repeat folded by `loops`, or one before
    /// `start`, is not sent either), so that all the cache holds up to it stays as it is. The steps of a request that left side by side share it. A message of the agent's there (a
    /// step that said something and called nothing) is no place for it: the request takes the line.
    /// When the request itself is no longer sent (it is before `start`; the first message always is), the
    /// line goes on the first message from `start` on that the view sends and that is not the agent's,
    /// before the step if one is, else after it (and then it does not say the reply follows): a line on a
    /// message not sent is lost.
    fn line_at(&self, i: usize, gone: &FxHashSet<usize>) -> usize {
        let s = &self.steps[i];
        let agent =
            |mi: usize| self.tr.msgs.get(mi).and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant");
        let sent = |mi: usize| !gone.contains(&mi) && !self.looped.contains(&mi);
        if s.a == 0 || s.a >= self.start {
            let mut mi = s.msgs[0].saturating_sub(1);
            while mi > s.a && (!sent(mi) || mi < self.start) {
                mi -= 1;
            }
            return if mi <= s.a || agent(mi) { s.a } else { mi };
        }
        (self.start..self.tr.msgs.len()).find(|&mi| sent(mi) && !agent(mi)).unwrap_or(0)
    }

    /// The steps out of the view leave it, and each run of them side by side says which calls left, on
    /// the message just before it (`line_at`), so that the agent can run them again. The reply that closed
    /// the request stays: it is what the agent took from that work, the way a note is. The steps brought
    /// back go as text in the message they came back at.
    /// Returns the oldest message it changed, if any.
    fn shut_lines(&mut self) -> Option<usize> {
        let mut out = FxHashSet::default();
        // the last step of each request: the line of the run that ends with it says the reply follows
        let mut last: FxHashMap<usize, usize> = FxHashMap::default();
        for (i, s) in self.steps.iter().enumerate() {
            if s.out || s.at.is_some() {
                out.extend(s.msgs.iter().copied());
            }
            last.insert(s.a, i);
        }
        // each run, by the message its line goes on and its request: the calls that left, the messages
        // that carry their outputs (what a `recall` is for), and whether it reaches the request's last step
        type Run<'s> = (IndexMap<&'s str, usize>, Vec<usize>, bool);
        let mut runs: IndexMap<(usize, usize), Run> = IndexMap::new();
        let mut brought: Vec<(usize, usize)> = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            if let (false, Some(at)) = (s.out, s.at) {
                brought.push((at, i));
            }
            // a step brought back still left its place: the line counts it, so it reads the same as when it
            // was written and what the cache holds before the latest messages stays as it is
            if !s.out && s.at.is_none() {
                continue;
            }
            // a step that left with the messages before `start` has no place in the view to name it from
            if s.msgs.iter().all(|&m| m < self.start) {
                continue;
            }
            let at = self.line_at(i, &out);
            // the reply follows only a line before the step, and only when the agent replied: a request left
            // for a newer one (`task`) has none
            let next = s.msgs.last().map_or(0, |m| m + 1);
            let replied = self.tr.msgs.get(next).is_some_and(|m| {
                m.get("role").and_then(Value::as_str) == Some("assistant")
                    && !blocks(m.get("content"))
                        .iter()
                        .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            });
            let (d, outs, end) = runs.entry((at, s.a)).or_default();
            for &m in &s.msgs {
                if self.tr.msgs[m].get("role").and_then(Value::as_str) != Some("assistant") {
                    outs.push(m);
                }
            }
            for what in &s.did {
                *d.entry(what.as_str()).or_insert(0) += 1;
            }
            *end |= last.get(&s.a) == Some(&i) && at < s.msgs[0] && replied;
        }
        // what came back to a message was there before the request it starts was closed: it goes first, so
        // the line that closes the request comes after it and the message only grows
        let mut shut: FxHashMap<usize, Vec<String>> = FxHashMap::default();
        brought.sort();
        for (at, i) in brought {
            shut.entry(at).or_default().push(self.step_text(i));
        }
        for ((at, _), (d, mut outs, end)) in runs {
            let calls: usize = d.values().sum();
            let mut said: Vec<String> = d
                .iter()
                .take(CLOSED_SHOWN)
                .map(|(what, &n)| if n > 1 { format!("{what} (×{n})") } else { what.to_string() })
                .collect();
            if d.len() > CLOSED_SHOWN {
                said.push(format!("… {} more", d.len() - CLOSED_SHOWN));
            }
            outs.sort_unstable();
            outs.dedup();
            let back = if self.recall_tool && !outs.is_empty() {
                let v = &outs;
                let name = |m: &usize| format!("m{:05}", m + 1);
                let mut names: Vec<String> = v.iter().take(RECALL_NAMED).map(name).collect();
                if v.len() > RECALL_NAMED {
                    names.push(format!("… {} more, to {}", v.len() - RECALL_NAMED, name(&v[v.len() - 1])));
                }
                format!(
                    " {} {}; recall brings {} back as it was.",
                    if v.len() == 1 { "The output is" } else { "The outputs are" },
                    names.join(", "),
                    if v.len() == 1 { "it" } else { "any of them" }
                )
            } else {
                String::new()
            };
            shut.entry(at).or_default().push(format!(
                "{MARK} {calls} tool call{} made for this request and {} output are left out of this context: {}.{}{back}",
                if calls == 1 { "" } else { "s" },
                if calls == 1 { "its" } else { "their" },
                said.join(" | "),
                if end { " The reply that closed the request follows." } else { "" }
            ));
        }
        let moved: Vec<usize> = out.symmetric_difference(&self.shut_out).copied().collect();
        let (going, coming): (Vec<usize>, Vec<usize>) = moved.iter().partition(|m| out.contains(m));
        self.tag_msgs(going, true, "close");
        self.tag_msgs(coming, false, "close");
        let relined: Vec<usize> =
            shut.keys().chain(self.shut.keys()).filter(|a| shut.get(a) != self.shut.get(a)).copied().collect();
        // the view changes only where it sends a message before or after: a step that leaves or comes back
        // from where the view does not send it (before `start`, in a topic left out) changes nothing there
        let before = self.visible();
        self.shut_out = out;
        self.vis = None;
        let after = self.visible();
        let sent: FxHashSet<usize> = before.iter().chain(after.iter()).copied().collect();
        let changed = moved.iter().chain(&relined).filter(|m| sent.contains(m)).min().copied();
        for mi in moved.into_iter().chain(relined) {
            self.built.remove(&mi);
        }
        let closed = self.steps.iter().filter(|s| s.out).count() as i64;
        self.stats.insert("closed_steps", closed);
        self.shut = shut;
        self.vis = None;
        changed
    }

    /// The last message is a tool result of at least `settle` tokens: the agent has just read something
    /// large, and that is when what it settled is cheapest to write down.
    fn big_read(&self, t: usize) -> bool {
        self.p.settle > 0
            && t >= 1
            && self.fz.by_msg(t - 1).iter().any(|&i| {
                let u = &self.tr.units[i];
                u.kind == UKind::Result && u.tokens >= self.p.settle
            })
    }

    /// Read, noted, let go: once a new request has begun, a tool result of an earlier request that a later
    /// note followed leaves the view whole. The note holds what the agent took from it; a result no note
    /// followed stays, since without one the agent reads it again. All leave in the same rewrite, so the
    /// cache breaks once per request rather than once per result. The oldest message one left from, if any.
    fn settle(&mut self, t: usize) -> Option<usize> {
        let n = self.goal.turns.len();
        let noted = self.last_note.filter(|_| self.p.settle > 0 && n >= 2)?;
        let end = noted.min(self.goal.turns[n - 1].start).min(t);
        self.fz.context(&self.tr, t);
        let mut settled = Vec::new();
        for mi in 0..end {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                let all = self.fz.chunks[i].len();
                if u.kind == UKind::Result
                    && u.saving > 0
                    && u.tokens >= SMALL_RESULT
                    && self.dropped.get(&i).map_or(0, |d| d.len()) < all
                {
                    settled.push((mi, i, all));
                }
            }
        }
        for &(_, i, all) in &settled {
            self.drop_as(i, 0..all, "settle");
        }
        self.bump("settled", settled.len() as i64);
        settled.first().map(|s| s.0)
    }

    /// Asks the agent for its findings and its plan (a `note` call, or the lines in its reply), at the end
    /// of the last tool result, once it has gone `nudge` steps without a note while the view loses what it
    /// read (it was rewritten, or the history is past the protected tail). At most once per `nudge` steps;
    /// only the latest request stays in the view, and it leaves once a note answers it.
    ///
    /// The request waits while the agent is between two parts of one file (its last call was a read with
    /// a range): in a measured run, a request between the first and the second part made the agent search
    /// the file instead of reading on, and the answers that depended on the rest were lost. The wait is
    /// bounded: with `n` the `nudge` setting, the request comes when
    /// `since_note >= n && since_nudge >= n && (!mid_read || since_note >= 2n)`.
    ///
    /// Returns the oldest earlier message it rebuilt, if any. With `nudge` off (only `settle` asks), the
    /// earlier requests stay as they were sent until the view changes anyway (`rewriting`).
    fn nudge(&mut self, t: usize, rewriting: bool) -> Option<usize> {
        let n = self.p.nudge;
        if !self.big_read(t) {
            if n <= 0 || self.since_note < n as usize || self.since_nudge < n as usize {
                return None;
            }
            if self.start <= 1 && self.dropped.is_empty() && t as i64 <= self.p.protect {
                return None;
            }
        }
        if self.since_note < 2 * n.max(1) as usize && self.mid_read(t) {
            return None;
        }
        let last = t - 1;
        let result = blocks(self.tr.msgs[last].get("content"))
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));
        if !result || self.nudged.contains(&last) {
            return None;
        }
        // the earlier request leaves with its message rebuilt plain: one request in the view, not a pile
        let mut oldest = None;
        if n > 0 || rewriting {
            for old in std::mem::take(&mut self.nudged) {
                self.built.remove(&old);
                oldest = Some(oldest.map_or(old, |o: usize| o.min(old)));
            }
        }
        self.nudged.insert(last);
        self.built.remove(&last);
        self.since_nudge = 0;
        self.bump("nudges", 1);
        oldest
    }

    /// The rules block: every rule whose message the view does not take (left, cut out by a topic
    /// switch, or in a history since rewritten).
    fn rules_block(&mut self) -> String {
        if self.p.rules == 0 || self.rules.list.is_empty() {
            return String::new();
        }
        let vis = self.visible();
        let seen: FxHashSet<usize> = vis.iter().copied().collect();
        self.rules.text(|r| !r.msg.is_some_and(|m| seen.contains(&m)))
    }

    /// The anchor: what `leave` worked out, and the rules block after it.
    fn compose(&mut self) {
        self.map_text = match (self.base_map.is_empty(), self.rules_shown.is_empty()) {
            (_, true) => self.base_map.clone(),
            (true, false) => self.rules_shown.clone(),
            (false, false) => format!("{}\n\n{}", self.base_map, self.rules_shown),
        };
        self.map_ver += 1;
    }

    /// Puts the rules block in the anchor when the rules it lists changed, and says whether it did. The
    /// anchor is the first message: a change to it makes the provider read the whole view again. So unless
    /// the view changes near its start anyway (`rewriting`), the block changes only when it lacks a rule
    /// the view no longer holds; a rule it lists while the view holds it again waits.
    fn refresh_rules(&mut self, rewriting: bool) -> bool {
        let block = self.rules_block();
        if block == self.rules_shown {
            return false;
        }
        let shown: FxHashSet<&str> = self.rules_shown.lines().collect();
        if !rewriting && block.lines().skip(1).all(|l| shown.contains(l)) {
            return false;
        }
        self.rules_shown = block;
        self.compose();
        true
    }

    /// Tokens the rules take past their soft budget: the findings, then the pinned files, give them up.
    fn rules_excess(&self) -> i64 {
        if self.p.rules == 0 || self.rules.list.is_empty() {
            return 0;
        }
        (est_str(&self.rules.text(|_| true)) - self.p.rules).max(0)
    }

    /// The calls and outputs of the agent message `mi` and the message after it, when the one is only calls
    /// and the other only their outputs: what a step that repeats it must match.
    fn step_sig(&self, mi: usize) -> Option<String> {
        let (a, r) = (self.tr.msgs.get(mi)?, self.tr.msgs.get(mi + 1)?);
        if a.get("role").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let kind = |b: &Value| b.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        let thought = |b: &&Value| kind(b).ends_with("thinking");
        let calls: Vec<Value> = blocks(a.get("content")).iter().filter(|b| !thought(b)).cloned().collect();
        let outs = blocks(r.get("content"));
        if calls.is_empty()
            || calls.iter().any(|b| kind(b) != "tool_use")
            || outs.iter().any(|b| kind(b) != "tool_result")
        {
            return None;
        }
        let calls = calls
            .iter()
            .map(|b| format!("{}{}", b.get("name").unwrap_or(&Value::Null), b.get("input").unwrap_or(&Value::Null)));
        let outs = outs.iter().map(|b| text_of(b.get("content")).into_owned());
        Some(calls.chain(outs).collect::<Vec<_>>().join("\n\u{0}"))
    }

    /// Takes in the steps that came since the last look (`Params.loops`): a step that repeats the one before
    /// it extends the run; the latest of a run before it leaves, unless it is the first, and the line moves. The
    /// oldest message it changed, if any.
    fn fold_loops(&mut self) -> Option<usize> {
        let n = self.tr.msgs.len();
        let (mut changed, mut left) = (Vec::new(), None);
        let mut mi = self.loops_seen.max(2);
        while mi + 1 < n {
            let same = match self.step_sig(mi) {
                Some(s) => self.step_sig(mi - 2).as_ref() == Some(&s),
                None => {
                    if self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant") {
                        self.run = None;
                    }
                    mi += 1;
                    continue;
                }
            };
            if !same {
                self.run = None;
                mi += 2;
                continue;
            }
            let (first, times) = match self.run {
                Some((first, last, times)) if last + 2 == mi => {
                    if last != first {
                        left = Some(left.map_or(last, |l: usize| l.min(last)));
                        self.looped.extend([last, last + 1]);
                        self.tag_msgs([last, last + 1], true, "loops");
                    }
                    self.loop_line.remove(&(last + 1));
                    changed.push(last + 1);
                    (first, times + 1)
                }
                _ => (mi - 2, 2),
            };
            self.run = Some((first, mi, times));
            self.loop_line.insert(
                mi + 1,
                format!("{MARK} This call has now run {times} times in a row with this same output; the runs between the first and this one are left out."),
            );
            changed.push(mi + 1);
            mi += 2;
        }
        self.loops_seen = mi;
        let oldest = changed.iter().copied().chain(left).min();
        if !changed.is_empty() {
            for c in changed {
                self.built.remove(&c);
            }
            self.vis = None;
            self.stats.insert("looped_steps", self.looped.len() as i64 / 2);
        }
        oldest
    }

    /// Tokens of the outputs after the agent's latest text or note: what it has read and not yet written about.
    /// The last message where the agent wrote: text of its own, or a note.
    fn last_written(&self) -> Option<usize> {
        self.tr
            .units
            .iter()
            .rev()
            .find(|u| u.kind == UKind::Text || (u.kind == UKind::Call && u.tool == NOTE_TOOL))
            .map(|u| u.msg)
    }

    /// The first message whose outputs `Params.still` keeps from a guess: the latest outputs since the agent
    /// last wrote, newest first, while they fit the budget. `usize::MAX` keeps none.
    fn still_from(&self, sizes: &[i64]) -> usize {
        if self.p.still <= 0 {
            return usize::MAX;
        }
        let first = self.last_written().map_or(0, |m| m + 1);
        let (mut from, mut kept) = (sizes.len(), 0);
        for mi in (first..sizes.len()).rev() {
            if self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result) {
                kept += sizes[mi];
                if kept > self.p.still {
                    break;
                }
            }
            from = mi;
        }
        from
    }

    /// The outputs the cap leaves out of its count (`Params.hold`): the ones the agent has not written about
    /// yet. With a budget for them (`Params.still`), only the latest that fit it: past it the older ones count,
    /// or a session where the agent never writes would grow past any cap.
    fn held(&self, sizes: &[i64]) -> i64 {
        let from = if self.p.still > 0 { self.still_from(sizes).min(sizes.len()) } else { 0 };
        let mut held = 0;
        for mi in (from..sizes.len()).rev() {
            let units = self.fz.by_msg(mi).iter().map(|&i| &self.tr.units[i]);
            let mut units = units.filter(|u| u.kind == UKind::Text || u.kind == UKind::Call);
            if units.any(|u| u.kind == UKind::Text || u.tool == NOTE_TOOL) {
                break;
            }
            if self.fz.by_msg(mi).iter().any(|&i| self.tr.units[i].kind == UKind::Result) {
                held += sizes[mi];
            }
        }
        held
    }

    /// The cap past which the view is cut, the share of it a cut brings the view down to, and whether
    /// the cut is priced with `base` instead of `Params.eoq`. A cut is an order of the economic order
    /// quantity: it costs reading again without the cache what the view keeps, and the tokens it lets
    /// grow cost `ratio` of that on every request until the next cut. `base` is what no cut takes (the
    /// first message and the protected tail). When even the deepest cut leaves no room for one more
    /// step, the cap as priced would cut every request a little and the provider would read the whole
    /// view again each time (a long text pasted into the latest request does it): the cut is then priced
    /// with what it leaves, so the view grows as sent until the cut makes room.
    fn eoq_cap(&self, k: usize, base: i64) -> (i64, f64, bool) {
        let r = self.p.ratio;
        if r <= 0.0 {
            return (0, self.floor, false);
        }
        let g = self.raw_tokens as f64 / k.max(1) as f64;
        let cap_of = |e: f64| e + if r < 1.0 { (2.0 * (1.0 - r) * e * g / r).sqrt() } else { 0.0 };
        let mut e = self.p.eoq as f64;
        let stuck = base as f64 + g > cap_of(e);
        if stuck {
            e = base as f64;
        }
        let cap = (cap_of(e) as i64).max(1);
        (cap, e / cap as f64, stuck)
    }

    fn prob(&self, x: &[f64]) -> f64 {
        if self.p.turn_head {
            MODEL.turn_prob(x)
        } else {
            MODEL.prob(x)
        }
    }

    /// The parts of the results in the view whose every line a later output in the view sends too, as
    /// (result, part), each with the parts that send those lines: the agent read the same lines again, and
    /// the later copy is the one it asked for. Only the lines a part still sends count: a part already left
    /// out carries nothing.
    fn copies(&self, vis: &[usize]) -> FxHashMap<(usize, usize), Vec<(usize, usize)>> {
        // each line sent below, with the latest part that sends it (`usize::MAX`: a line no part holds,
        // which never leaves)
        let mut later: FxHashMap<String, (usize, usize)> = FxHashMap::default();
        let mut out = FxHashMap::default();
        for &mi in vis.iter().rev() {
            for &i in self.fz.by_msg(mi).iter().rev() {
                if self.tr.units[i].kind != UKind::Result {
                    continue;
                }
                let text = self.tr.unit_text(i);
                let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
                let have = self.dropped.get(&i);
                let mut owner: Vec<Option<usize>> = vec![Some(usize::MAX); lines.len()];
                for (j, c) in self.fz.chunks[i].iter().enumerate() {
                    let (a, b) = (c.start.max(0) as usize, (c.end.max(0) as usize).min(lines.len()));
                    let gone = have.is_some_and(|h| h.contains(&j));
                    owner[a.min(b)..b].iter_mut().for_each(|o| *o = (!gone).then_some(j));
                    if gone {
                        continue;
                    }
                    let own = &lines[a.min(b)..b];
                    if own.iter().any(|l| !l.is_empty()) && own.iter().all(|l| l.is_empty() || later.contains_key(*l)) {
                        let mut by: Vec<(usize, usize)> =
                            own.iter().filter(|l| !l.is_empty()).map(|l| later[*l]).collect();
                        by.sort_unstable();
                        by.dedup();
                        out.insert((i, j), by);
                    }
                }
                for (l, o) in lines.iter().zip(owner) {
                    if let (false, Some(j)) = (l.is_empty(), o) {
                        later.entry(l.to_string()).or_insert((i, j));
                    }
                }
            }
        }
        out
    }

    fn evict(&mut self, t: usize, k: usize, sizes: &[i64], free: bool) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let copies = if self.p.sup > 0 { self.copies(&vis) } else { FxHashMap::default() };
        // a part that sends the lines of a copy left out (now or before) stays: they would be gone
        let mut carried = self.carrying.clone();
        // past the cap, a read a later edit made out of date goes with the copies, before any guess
        let stale = if free { self.stale_reads(&vis) } else { FxHashSet::default() };
        // what the agent read and has not written about yet is what it is working from: no guess takes it
        let still = self.still_from(sizes)..usize::MAX;
        // a part whose lines a later copy points to only moves down to that copy if it leaves: what it
        // saves is the share of its lines no copy points to; the lines it sends as a pointer to an earlier
        // output (`dedup`) cost next to nothing already, and leaving saves none of them either
        let mut pointed = self.pointed(|_| true);
        for (u, lines) in self.pointing(&vis) {
            let n = self.shown.get(&u).map_or(0, Vec::len);
            let held = pointed.entry(u).or_insert_with(|| vec![false; n]);
            held.iter_mut().take(lines.1).skip(lines.0).for_each(|l| *l = true);
        }
        // the latest reply's reasoning goes back with its calls (a provider refuses a call without it), so no
        // tail short enough to leave that reply open lets it go
        let last_reply =
            (0..t).rev().find(|&m| self.tr.msgs[m].get("role").and_then(Value::as_str) == Some("assistant"));
        let mut proposal: Vec<Proposal> = Vec::new();
        for &mi in vis.iter() {
            if mi as i64 >= t as i64 - self.p.protect {
                break;
            }
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                if u.kind == UKind::Thinking && Some(mi) == last_reply {
                    continue;
                }
                // a short output is a measurement or a verdict: its stub saves next to nothing, and running
                // the tool again to see it costs a whole request
                if u.saving == 0 || (u.kind == UKind::Result && u.tokens < SMALL_RESULT) {
                    continue;
                }
                let chunks = &self.fz.chunks[i];
                let have = self.dropped.get(&i);
                if have.map_or(0, |h| h.len()) == chunks.len() {
                    continue;
                }
                let more: Vec<usize> = (0..chunks.len())
                    .filter(|j| {
                        !have.is_some_and(|h| h.contains(j))
                            && (copies.contains_key(&(i, *j))
                                || (!carried.contains(&(i, *j))
                                    && (stale.contains(&i)
                                        || (!still.contains(&mi)
                                            && self.prob(&self.fz.vector(&self.tr, i, *j)) < self.p.threshold))))
                    })
                    .collect();
                let free = |j: usize| -> i64 {
                    let Some(p) = pointed.get(&i) else { return chunks[j].tokens };
                    let n = p.len() as i64;
                    let at = |x: i64| (if x < 0 { (n + x).max(0) } else { x.min(n) }) as usize;
                    let (a, b) = (at(chunks[j].start), at(chunks[j].end));
                    let span = b.saturating_sub(a);
                    let held = p[a..b.max(a)].iter().filter(|x| **x).count();
                    if span == 0 {
                        chunks[j].tokens
                    } else {
                        chunks[j].tokens * (span - held) as i64 / span as i64
                    }
                };
                let more: Vec<usize> = more.into_iter().filter(|&j| free(j) > 0).collect();
                for j in &more {
                    carried.extend(copies.get(&(i, *j)).into_iter().flatten().copied());
                }
                if !more.is_empty() {
                    let tokens = more.iter().map(|&j| free(j)).sum();
                    proposal.push((self.tr.units[i].msg, i, more, tokens));
                }
            }
        }
        if proposal.is_empty() {
            self.held.clear();
            self.held_at = k;
            return None;
        }
        let best_at = if free { 0 } else { self.rent(t, k, sizes, &proposal)? };
        for (_, i, more, _) in &proposal[best_at..] {
            for &j in more {
                let rule = if copies.contains_key(&(*i, j)) {
                    "sup"
                } else if stale.contains(i) {
                    "stale"
                } else if free {
                    "cap_guess"
                } else {
                    "rent"
                };
                self.drop_one(*i, j, rule);
            }
            self.bump("dropped_chunks", more.len() as i64);
            for j in more {
                if let Some(by) = copies.get(&(*i, *j)) {
                    self.carrying.extend(by.iter().copied());
                    self.copied.insert((*i, *j));
                    self.bump("sup_chunks", 1);
                }
                self.held.remove(&(*i, *j));
            }
        }
        if free {
            self.held.clear();
            self.held_at = k;
        }
        self.bump("rewrites", 1);
        Some(proposal[best_at].0)
    }

    /// Past the cap: lets parts of results go, the safest first, until the view is down to ``target``.
    /// A part whose every line a later output in the view sends too is lost by nobody, and neither is a
    /// read a later edit of its file made out of date (`Params.stale`); after those, the parts least
    /// likely to be needed. What the oldest messages hold is no less needed for being old: the file read
    /// first is often the one the work is about.
    fn need(&mut self, t: usize, sizes: &[i64], target: f64, only_copies: bool) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let stale = self.stale_reads(&vis);
        // the lines each visible output still sends, the latest first, with the part that sends them
        let mut later: FxHashMap<String, (usize, usize)> = FxHashMap::default();
        let mut carriers: FxHashMap<(usize, usize), Vec<(usize, usize)>> = FxHashMap::default();
        let mut parts: Vec<(f64, usize, usize, usize, i64, bool)> = Vec::new();
        for &mi in vis.iter().rev() {
            for &i in self.fz.by_msg(mi).iter().rev() {
                let u = &self.tr.units[i];
                if u.kind != UKind::Result {
                    continue;
                }
                let text = self.tr.unit_text(i).into_owned();
                let lines: Vec<&str> = text.split('\n').collect();
                let have = self.dropped.get(&i).cloned().unwrap_or_default();
                let chunks = &self.fz.chunks[i];
                let open = (mi as i64) < t as i64 - self.p.protect && u.saving > 0 && u.tokens >= SMALL_RESULT;
                for (j, c) in chunks.iter().enumerate() {
                    if have.contains(&j) {
                        continue;
                    }
                    let own = &lines[c.start.max(0) as usize..(c.end.max(0) as usize).min(lines.len())];
                    if open {
                        let copy = own.iter().all(|l| l.trim().is_empty() || later.contains_key(l.trim()));
                        if copy {
                            let mut by: Vec<(usize, usize)> =
                                own.iter().filter_map(|l| later.get(l.trim()).copied()).collect();
                            by.sort_unstable();
                            by.dedup();
                            carriers.insert((i, j), by);
                        }
                        if only_copies && !copy && !stale.contains(&i) {
                            for l in own.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                                later.entry(l.to_string()).or_insert((i, j));
                            }
                            continue;
                        }
                        let score =
                            if copy || stale.contains(&i) { -1.0 } else { self.prob(&self.fz.vector(&self.tr, i, j)) };
                        parts.push((score, mi, i, j, c.tokens, copy));
                    }
                    for l in own.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                        later.entry(l.to_string()).or_insert((i, j));
                    }
                }
            }
        }
        // the copies first: a part that sends a copy's lines may itself be one to leave, and it stays once
        // the copy has gone
        parts.sort_by(|a, b| a.0.total_cmp(&b.0).then(b.5.cmp(&a.5)).then(a.1.cmp(&b.1)));
        let mut carried = self.carrying.clone();
        let mut total = sizes.iter().sum::<i64>() as f64;
        let mut first: Option<usize> = None;
        for (score, mi, i, j, tokens, copy) in parts {
            if total <= target {
                break;
            }
            if !copy && carried.contains(&(i, j)) {
                continue;
            }
            let (rule, stat) = match (copy, score < 0.0) {
                (true, _) => ("cap_copies", "need_copies"),
                (false, true) => ("cap_stale", "need_stale"),
                _ => ("cap_need", "need_chunks"),
            };
            self.drop_one(i, j, rule);
            self.held.remove(&(i, j));
            self.bump(stat, 1);
            if copy {
                let by = carriers.remove(&(i, j)).unwrap_or_default();
                carried.extend(by.iter().copied());
                self.carrying.extend(by);
                self.copied.insert((i, j));
            }
            total -= tokens as f64;
            first = Some(first.map_or(mi, |f| f.min(mi)));
        }
        if first.is_some() {
            self.bump("rewrites", 1);
        }
        first
    }

    /// The read results of the messages `vis` that a later edit of their file made out of date
    /// (`Params.stale`; empty when it is off).
    fn stale_reads(&self, vis: &[usize]) -> FxHashSet<usize> {
        if self.p.stale == 0 || self.edits.is_empty() {
            return FxHashSet::default();
        }
        vis.iter().flat_map(|&mi| self.fz.by_msg(mi)).copied().filter(|&i| self.superseded_by(i).is_some()).collect()
    }

    /// Past the cap: keeps the parts of results that tell the most per token and lets the rest go, until
    /// the view is down to ``target``. A part is worth the words it holds that nothing kept so far holds,
    /// each weighed by [`Featurizer::worth`] (rare words more, the words the current step asks about
    /// more again), so a second copy of something kept is worth nothing, whatever its age. Greedy by worth
    /// per token: the parts kept tell at least 1 − 1/e of what the best choice of the same size tells.
    fn cover(&mut self, t: usize, sizes: &[i64], target: f64) -> Option<usize> {
        self.fz.context(&self.tr, t);
        let vis = self.visible();
        let mut known: FxHashSet<Sym> = FxHashSet::default();
        let mut parts: Vec<(usize, usize, usize, i64)> = Vec::new();
        let mut open_tokens = 0i64;
        for &mi in vis.iter() {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                let open = u.kind == UKind::Result
                    && (mi as i64) < t as i64 - self.p.protect
                    && u.saving > 0
                    && u.tokens >= SMALL_RESULT;
                let have = self.dropped.get(&i);
                for (j, c) in self.fz.chunks[i].iter().enumerate() {
                    if have.is_some_and(|h| h.contains(&j)) {
                        continue;
                    }
                    // a part that sends the lines of a copy left out stays, as what is known
                    if open && !self.carrying.contains(&(i, j)) {
                        parts.push((mi, i, j, c.tokens));
                        open_tokens += c.tokens;
                    } else if u.kind == UKind::Result {
                        // what a request or a reply names is asked about, not told: only material covers a word
                        known.extend(c.terms.iter().copied());
                    }
                }
            }
        }
        let fixed = sizes.iter().sum::<i64>() - open_tokens;
        let mut room = target - fixed as f64;
        if open_tokens as f64 <= room {
            return None;
        }
        let worth = |known: &FxHashSet<Sym>, (_, i, j, tokens): &(usize, usize, usize, i64)| -> f64 {
            let gain: f64 =
                self.fz.chunks[*i][*j].terms.iter().filter(|w| !known.contains(w)).map(|&w| self.fz.worth(w)).sum();
            gain / (*tokens).max(1) as f64
        };
        // lazy greedy: a part's worth only falls as more is kept, so a stale top is weighed again; of two
        // parts worth the same, the later one stays
        let mut heap: BinaryHeap<(Score, usize)> =
            parts.iter().enumerate().map(|(n, p)| (Score(worth(&known, p)), n)).collect();
        let mut keep = vec![false; parts.len()];
        while let Some((Score(s), n)) = heap.pop() {
            let now = worth(&known, &parts[n]);
            if now < s && heap.peek().is_some_and(|(Score(top), _)| now < *top) {
                heap.push((Score(now), n));
                continue;
            }
            let tokens = parts[n].3 as f64;
            if tokens <= room {
                room -= tokens;
                keep[n] = true;
                known.extend(self.fz.chunks[parts[n].1][parts[n].2].terms.iter().copied());
            }
        }
        let mut first: Option<usize> = None;
        for (n, &(mi, i, j, _)) in parts.iter().enumerate() {
            if keep[n] {
                continue;
            }
            self.drop_one(i, j, "cap_cover");
            self.held.remove(&(i, j));
            self.bump("cover_chunks", 1);
            first = Some(first.map_or(mi, |f| f.min(mi)));
        }
        if first.is_some() {
            self.bump("rewrites", 1);
        }
        first
    }

    fn rent(&mut self, t: usize, k: usize, sizes: &[i64], proposal: &[Proposal]) -> Option<usize> {
        let since = self.held_at;
        let mut held = FxHashMap::default();
        for (_, i, more, _) in proposal {
            let chunks = &self.fz.chunks[*i];
            let age = (k as i64 - since.max(self.born.get(*i).copied().unwrap_or(0)) as i64).max(0);
            for &j in more {
                let old = self.held.get(&(*i, j)).copied().unwrap_or(0.0);
                held.insert((*i, j), old + self.p.ratio * chunks[j].tokens as f64 * age as f64);
            }
        }
        self.held = held;
        self.held_at = k;
        let mut cum = Vec::with_capacity(sizes.len() + 1);
        cum.push(0i64);
        for s in sizes {
            cum.push(cum[cum.len() - 1] + s);
        }
        let sent = self.tr.steps.last().map_or(t, |&s| s.min(t));
        // what the cache no longer holds (`Params.pockets` 2) is read again cut or not: a cut there costs nothing,
        // and what it saves there makes no cut before it cheaper
        let warm = if self.p.pockets >= 2 { self.warm } else { usize::MAX };
        let (mut best_at, mut saved, mut paid, mut bought) = (None, 0i64, 0.0f64, (0.0f64, 0.0f64));
        for n in (0..proposal.len()).rev() {
            let (f, i, more, tokens) = &proposal[n];
            if *f < warm {
                saved += tokens;
            }
            paid += more.iter().map(|j| self.held[&(*i, *j)]).sum::<f64>();
            if n > 0 && proposal[n - 1].0 == *f {
                continue;
            }
            let rest = (cum[(*f).max(warm).min(sent)] - cum[(*f).min(sent)] - saved).max(0);
            let price = self.p.margin * (1.0 - self.p.ratio) * rest as f64;
            let need = price;
            if paid >= need && paid - bought.0 >= need - bought.1 {
                best_at = Some(n);
                bought = (paid, need);
            }
        }
        best_at
    }

    /// The tokens the view may take in the model's window (0: no limit).
    pub fn set_budget(&mut self, tokens: i64) {
        self.budget = tokens.max(0);
    }

    /// The window: a view over [`Engine::set_budget`] would be refused by the provider, so the oldest
    /// messages leave until it fits (they go to the ledger, as past the cap), the protected tail first
    /// kept and then given up but for the latest message. What one message alone holds past the
    /// window is paged when the view is rendered (`Curator::view`). Whether any message left.
    fn fit(&mut self) -> bool {
        if self.budget <= 0 {
            return false;
        }
        let sizes = self.sizes_now();
        if sizes.iter().sum::<i64>() <= self.budget {
            return false;
        }
        let mut left = self.leave(&sizes, self.budget as f64);
        let sizes = self.sizes_now();
        if sizes.iter().sum::<i64>() > self.budget {
            let protect = std::mem::replace(&mut self.p.protect, 1);
            left |= self.leave(&sizes, self.budget as f64);
            self.p.protect = protect;
        }
        self.bump("window_fits", 1);
        left
    }

    fn leave(&mut self, sizes: &[i64], target: f64) -> bool {
        let t = self.tr.msgs.len() as i64;
        let mut total: i64 = sizes.iter().sum();
        let mut new_start = self.start;
        let mut mi = self.start;
        while (mi as i64) < (t - self.p.protect).min(t - 1) && total as f64 > target {
            total -= sizes[mi];
            mi += 1;
            if self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant") {
                new_start = mi;
            }
        }
        if new_start == self.start {
            return false;
        }
        // a message that left the view is not built again: its built form goes with it
        for mi in self.start.max(1)..new_start {
            self.built.remove(&mi);
        }
        for mi in self.start..new_start {
            self.gone_tokens += self.sizes[mi];
            for &i in self.fz.by_msg(mi) {
                if self.tr.units[i].kind != UKind::Call {
                    continue;
                }
                let Some(info) = self.fz.info.get(&i) else { continue };
                match (info.kind, &info.path) {
                    (Kind::Write, Some(p)) if !p.is_empty() => *self.gone.entry(p.clone()).or_insert(0) += 1,
                    (Kind::Exec, _) => {
                        self.gone_commands += 1;
                        for w in shell_writes(&command_of(self.tr.block(i))) {
                            *self.gone.entry(w).or_insert(0) += 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.bump("left_messages", (new_start - self.start) as i64);
        self.tag_msgs(self.start..new_start, true, "leave");
        if self.p.ledger != 0 {
            // the pool keeps its latest `POOL` lines: the messages that left are read from the newest back,
            // and the read stops once they alone fill it (a long history leaving at once is not read whole)
            let mut batches: Vec<Vec<(usize, String, Set, f64)>> = Vec::new();
            let mut filled = 0;
            for mi in (self.start..new_start).rev() {
                if filled >= POOL {
                    break;
                }
                let mut batch = Vec::new();
                for &i in self.fz.by_msg(mi) {
                    if self.tr.units[i].kind == UKind::Thinking {
                        continue;
                    }
                    let text = self.tr.unit_text(i).into_owned();
                    // what was said weighs twice; the body of a file read or written is on disk and weighs less
                    let info = self.fz.info.get(&i);
                    let kind = self.tr.units[i].kind;
                    let said = matches!(kind, UKind::Text | UKind::User)
                        || (kind == UKind::Call && get_str(self.tr.block(i), "name") == NOTE_TOOL);
                    let w = match kind {
                        _ if said => SAID,
                        UKind::Result if info.is_some_and(|x| x.kind == Kind::Read && !x.error && x.path.is_some()) => {
                            ON_DISK
                        }
                        UKind::Call if info.is_some_and(|x| x.kind == Kind::Write && x.path.is_some()) => ON_DISK,
                        _ => 1.0,
                    };
                    // what was said keeps its rows of numbers whole: they are the values it stated
                    let mut lines = if said {
                        said_lines(&text, &mut self.int)
                    } else {
                        fold_forms(lines_of(&text, &mut self.int))
                    };
                    if kind == UKind::Call {
                        let old = self.superseded(i, mi);
                        if !old.is_empty() {
                            lines.retain(|(ln, _)| !old.contains(ln));
                        }
                    }
                    for (ln, ws) in lines {
                        batch.push((mi, ln, ws, w));
                    }
                }
                filled += batch.len();
                batches.push(batch);
            }
            for batch in batches.into_iter().rev() {
                self.pool.extend(batch);
            }
            if self.pool.len() > POOL {
                self.pool.drain(..self.pool.len() - POOL);
            }
        }
        self.harvest(self.start, new_start);
        self.start = new_start;
        // a line on a message that left goes back on its request: the view is read again whole anyway
        if !self.shut.is_empty() {
            self.shut_lines();
        }
        self.vis = None;
        let kept;
        (self.base_map, kept) = self.map();
        self.rules_shown = self.rules_block();
        self.kept_tokens = kept + est_str(&self.rules_shown);
        if self.p.ledger != 0 {
            // the ledger takes what the rules, the findings and the pinned files leave of 80% of the view
            let budget = self.p.ledger.min((0.8 * target) as i64 - self.kept_tokens);
            let text = self.ledger_text(budget);
            if !text.is_empty() {
                self.base_map += "\n\n";
                self.base_map += &text;
            }
        }
        self.compose();
        true
    }

    /// The lines that call ``i`` (in message ``mi``) wrote whole to a file written whole again later.
    fn superseded(&self, i: usize, mi: usize) -> FxHashSet<String> {
        let b = self.tr.block(i);
        let info = self.fz.info.get(&i);
        let mut bodies: Vec<(String, String)> = match info.map(|x| x.kind) {
            Some(Kind::Write) => full_write(get_str(b, "name"), b.get("input"))
                .map(|t| (info.and_then(|x| x.path.clone()).unwrap_or_default(), t.to_string()))
                .into_iter()
                .collect(),
            Some(Kind::Exec) => heredoc_writes(&command_of(b)),
            _ => Vec::new(),
        };
        bodies.retain(|(p, _)| self.wrote.iter().any(|(k, w)| w.mi > mi && same_file(k, p)));
        bodies.iter().flat_map(|(_, t)| t.split('\n').map(py::squash_ws)).collect()
    }

    fn ledger_text(&mut self, budget: i64) -> String {
        let t = self.tr.msgs.len();
        let mut names = self.names.take().unwrap_or_default();
        names.extend(&self.tr, t);
        let mut covered: FxHashSet<Sym> = FxHashSet::default();
        for &mi in self.visible().iter() {
            for &i in self.fz.by_msg(mi) {
                if self.tr.units[i].kind != UKind::Thinking {
                    covered.extend(self.tr.units[i].words.iter().copied());
                }
            }
        }
        let mut p: FxHashMap<Sym, f64> = FxHashMap::default();
        for (_, _, ws, _) in &self.pool {
            for &w in ws {
                if !p.contains_key(&w) && !covered.contains(&w) && names.stats.contains_key(&w) {
                    p.insert(w, prior(&names.features(w, t, &self.int)));
                }
            }
        }
        self.names = Some(names);
        let head = if self.recall_tool {
            format!("{MARK} Facts from the messages that moved out, the ones most likely to be needed again (recall m##### brings a whole message back as it was):")
        } else {
            format!("{MARK} Facts from the messages that moved out, the ones most likely to be needed again:")
        };
        // the header is part of the text: the lines take what it leaves of the budget
        let budget = budget - est_str(&head) - 1;
        let taken =
            if budget > 0 { select(&self.pool, &p, &covered, budget, self.p.least, PER_MESSAGE) } else { Vec::new() };
        self.bump("ledger_lines", taken.len() as i64);
        if taken.is_empty() {
            return String::new();
        }
        let mut lines = vec![head];
        lines.extend(taken.iter().map(|(mi, ln)| format!("m{:05} {}", mi + 1, ln)));
        lines.join("\n")
    }

    /// What the anchor keeps of messages ``a`` to ``b`` (leaving the view now), in their own words: the
    /// agent's marked findings and plan (``notes``) and the files the person named, as last read
    /// (``pin``). The rules are taken as messages arrive (`take_rules`).
    fn harvest(&mut self, a: usize, b: usize) {
        for mi in a..b {
            for i in self.fz.by_msg(mi).to_vec() {
                let kind = self.tr.units[i].kind;
                let note = kind == UKind::Text || (kind == UKind::Call && self.tr.units[i].tool == NOTE_TOOL);
                if note && self.p.notes != 0 {
                    let text = self.tr.unit_text(i);
                    for c in NOTE_LINE.captures_all(&text) {
                        let span = |g: usize| c[g].map_or("", |(x, y)| &text[x..y]);
                        let line = py::head(&py::squash_ws(span(2)), NOTE_CHARS).to_string();
                        if span(1) == "Rule" {
                            continue;
                        }
                        if span(1) == "Plan" {
                            self.plan = Some((mi, line));
                        } else if self.findings.iter().all(|(_, x)| *x != line) {
                            self.findings.push((mi, line));
                        }
                    }
                } else if kind == UKind::Result && self.p.pin != 0 {
                    let Some(info) = self.fz.info.get(&i) else { continue };
                    let Some(path) = info.path.as_deref() else { continue };
                    if info.kind != Kind::Read || path.is_empty() || info.error {
                        continue;
                    }
                    let base = basename(path);
                    if base.is_empty() {
                        continue;
                    }
                    let asked = |s: &str| self.int.find(s).is_some_and(|w| self.asked.contains(&w));
                    if !(asked(path) || asked(base)) {
                        continue;
                    }
                    let text = self.tr.unit_text(i);
                    let text = py::strip(&text);
                    if !text.is_empty() {
                        // the first n characters, n = 4 * floor(pin / 2); a negative n counts from the end
                        let n = 4 * self.p.pin.div_euclid(2);
                        let n = if n >= 0 { n as usize } else { (py::len(text) as i64 + n).max(0) as usize };
                        self.pins.insert(file_key(path), (mi, path.to_string(), py::head(text, n).to_string()));
                    }
                }
            }
        }
        // a file written after it was read is not as it was read: the pin goes (the next read pins it again)
        let written = &self.written;
        self.pins.retain(|key, (mi, _, _)| !written.iter().any(|(k, w)| w > mi && same_file(k, key)));
        // the rules are never dropped: past their budget, the findings and then the pinned files yield
        let over = self.rules_excess();
        let (notes, pin) = (self.p.notes - over, self.p.pin - (over - self.p.notes).max(0));
        while self.findings.len() > 1 && est_str(&self.notes_text()) > notes {
            self.findings.remove(0);
        }
        // the reads and the writes share the budget, the newest first; scratch files go before the rest
        loop {
            let shown: Vec<usize> = (0..self.wrote.len()).filter(|&k| self.wrote[k].mi < b).collect();
            if self.pins.len() + shown.len() <= 1 || est_str(&self.pins_text()) + est_str(&self.wrote_text(b)) <= pin {
                break;
            }
            let oldest_write = shown.iter().copied().min_by_key(|&k| self.wrote[k].mi);
            let scratch_write = shown.iter().copied().find(|&k| scratch(self.wrote.get_index(k).map_or("", |x| x.0)));
            let oldest_read = (0..self.pins.len()).min_by_key(|&k| self.pins[k].0);
            match (scratch_write, oldest_write, oldest_read) {
                (Some(k), _, _) => {
                    self.wrote.shift_remove_index(k);
                }
                (None, Some(w), Some(r)) if self.wrote[w].mi < self.pins[r].0 => {
                    self.wrote.shift_remove_index(w);
                }
                (None, _, Some(r)) => {
                    self.pins.shift_remove_index(r);
                }
                (None, Some(w), None) => {
                    self.wrote.shift_remove_index(w);
                }
                (None, None, None) => break,
            }
        }
    }

    /// The files the agent wrote in the messages before ``upto`` (gone from the view), as last
    /// written, the newest first.
    fn wrote_text(&self, upto: usize) -> String {
        let mut parts = vec![format!("{MARK} Files you wrote, as last written (whole):")];
        for w in self.wrote.values().rev().filter(|w| w.mi < upto) {
            let stale =
                w.edited.map_or(String::new(), |e| format!("; edited later in m{:05}: re-read before trusting", e + 1));
            parts.push(format!("--- {} (written in m{:05}{stale}) ---\n{}", w.path, w.mi + 1, w.text));
        }
        if parts.len() == 1 {
            return String::new();
        }
        parts.join("\n")
    }

    fn notes_text(&self) -> String {
        if self.findings.is_empty() && self.plan.is_none() {
            return String::new();
        }
        let mut lines =
            vec![format!("{MARK} What you established in them, in your own words (do not redo this work):")];
        lines.extend(self.findings.iter().map(|(mi, ln)| format!("m{:05} Finding: {ln}", mi + 1)));
        if let Some((mi, ln)) = &self.plan {
            lines.push(format!("m{:05} Plan: {ln}", mi + 1));
        }
        lines.join("\n")
    }

    fn pins_text(&self) -> String {
        if self.pins.is_empty() {
            return String::new();
        }
        let mut parts = vec![format!(
            "{MARK} Files the request names, as last read (whole; read one again only if it may have changed):"
        )];
        let mut pins: Vec<&(usize, String, String)> = self.pins.values().collect();
        pins.sort();
        for (mi, path, text) in pins {
            parts.push(format!("--- {path} (read in m{:05}) ---\n{text}", mi + 1));
        }
        parts.join("\n")
    }

    fn unit_text_at(&self, mi: usize, kind: UKind) -> String {
        let parts: Vec<String> = self
            .fz
            .by_msg(mi)
            .iter()
            .filter(|&&i| self.tr.units[i].kind == kind)
            .map(|&i| self.tr.unit_text(i).into_owned())
            .collect();
        parts.join("\n")
    }

    /// The map text, with the findings and the pinned files after it, and the tokens of those.
    fn map(&self) -> (String, i64) {
        let mut lines = vec![format!(
            "{MARK} Messages 2 to {} of this conversation (about {} tokens) were moved out of the context to keep it small. They do not come back by themselves: what is kept of them is below; to see more of a file or an output again, read the file or run the command again.",
            self.start,
            thousands(self.gone_tokens)
        )];
        let reqs = &self.fz.requests;
        let n = self.tr.msgs.len();
        let end_of = |x: usize| reqs.get(x + 1).copied().unwrap_or(n);
        let upto = bisect_left(reqs, &self.start);
        let requests: Vec<usize> = (0..upto)
            .filter(|&x| {
                reqs[x] > 0 || !self.outcome(0, end_of(x)).is_empty() || !self.fixes(0, end_of(x), None).is_empty()
            })
            .collect();
        if !requests.is_empty() {
            lines.push("Requests the user made in them, each with the agent's closing reply (→):".into());
            let shown: Vec<Option<usize>> = if requests.len() <= 12 {
                requests.iter().map(|&x| Some(x)).collect()
            } else {
                let mut v: Vec<Option<usize>> = requests[..2].iter().map(|&x| Some(x)).collect();
                v.push(None);
                v.extend(requests[requests.len() - 10..].iter().map(|&x| Some(x)));
                v
            };
            for x in shown {
                let Some(x) = x else {
                    lines.push(format!("- … {} more …", requests.len() - 12));
                    continue;
                };
                let r = reqs[x];
                let ask = if r == 0 {
                    "(the first request, above)".to_string()
                } else {
                    let s = py::squash_ws(&self.unit_text_at(r, UKind::User));
                    if py::len(&s) > 300 {
                        format!("{}…", py::head(&s, 300))
                    } else {
                        s
                    }
                };
                let out = self.outcome(r, end_of(x));
                let mut l = format!("- {ask}");
                if !out.is_empty() {
                    l += &format!("\n  → {out}");
                }
                l += &self.fixes(r, end_of(x), None);
                lines.push(l);
            }
        }
        if !self.gone.is_empty() {
            let mut common: Vec<(&String, &i64)> = self.gone.iter().collect();
            common.sort_by(|a, b| b.1.cmp(a.1));
            let files: Vec<String> = common
                .iter()
                .take(20)
                .map(|(p, n)| if **n > 1 { format!("{p} ×{n}") } else { p.to_string() })
                .collect();
            lines.push(format!("Files changed in them: {}.", files.join(", ")));
        }
        if self.gone_commands != 0 {
            lines.push(format!("Commands run in them: {}.", self.gone_commands));
        }
        let mut text = lines.join("\n");
        let mut kept = 0;
        for block in [self.notes_text(), self.pins_text(), self.wrote_text(self.start)] {
            if !block.is_empty() {
                text += "\n\n";
                text += &block;
                kept += est_str(&block);
            }
        }
        (text, kept)
    }

    fn outcome(&self, r: usize, end: usize) -> String {
        let last =
            (r + 1..end).rev().find(|&mi| self.tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant"));
        let Some(last) = last else { return String::new() };
        if last >= self.start || self.fz.by_msg(last).iter().any(|&i| self.tr.units[i].kind == UKind::Call) {
            return String::new();
        }
        let x = py::squash_ws(&self.unit_text_at(last, UKind::Text));
        if py::len(&x) > OUTCOME_CHARS {
            format!("{}…", py::head(&x, OUTCOME_CHARS))
        } else {
            x
        }
    }

    fn fixes(&self, r: usize, end: usize, limit: Option<usize>) -> String {
        let mut changed: Vec<String> = Vec::new();
        let mut fixed: Vec<String> = Vec::new();
        let mut tests: Vec<String> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        // each failing run with its test run (`test_run`): only a run of that command, or of a broader one, says
        // whether its tests pass now
        let mut failures: Vec<(String, String)> = Vec::new();
        let mut commands: FxHashMap<String, String> = FxHashMap::default();
        let mut unnamed: i64 = 0;
        let stop = end.min(limit.unwrap_or(self.start));
        for mi in r..stop {
            for &i in self.fz.by_msg(mi) {
                let (kind, path, error) = match self.fz.info.get(&i) {
                    Some(x) => (Some(x.kind), x.path.clone(), x.error),
                    None => (None, None, false),
                };
                let b = self.tr.block(i);
                match self.tr.units[i].kind {
                    UKind::Call => {
                        let command = if kind == Some(Kind::Exec) { command_of(b) } else { String::new() };
                        commands.insert(id_key(b.get("id")), command.clone());
                        let ws = match (kind, path) {
                            (Some(Kind::Write), Some(p)) if !p.is_empty() => vec![p],
                            _ => shell_writes(&command),
                        };
                        for w in ws {
                            if !changed.contains(&w) {
                                changed.push(w.clone());
                            }
                            if !failures.is_empty() && !pending.contains(&w) {
                                pending.push(w);
                            }
                        }
                    }
                    UKind::Result if kind == Some(Kind::Exec) => {
                        let cmd = commands.get(&id_key(b.get("tool_use_id"))).map_or("", |s| s.as_str());
                        if !TEST_COMMAND.is_match(cmd) {
                            continue;
                        }
                        let cmd = test_run(cmd);
                        let cmd = cmd.as_str();
                        let text = self.tr.unit_text(i);
                        if error || ERROR_MARKS.is_match(&text) || TEST_FAILED.is_match(&text) {
                            let ls: Vec<&str> =
                                py::splitlines(&text).into_iter().filter(|x| FAILURE_LINE.is_match(x)).collect();
                            let f = ls.join("\n");
                            // a test failing again no longer passes
                            let again = failed_tests(&f);
                            tests.retain(|t| !again.contains(&t.as_str()));
                            failures.push((cmd.to_string(), f));
                        } else {
                            let (done, rest): (Vec<_>, Vec<_>) = failures.drain(..).partition(|(c, _)| covers(cmd, c));
                            failures = rest;
                            if done.is_empty() {
                                continue;
                            }
                            let done: Vec<String> = done.into_iter().map(|(_, f)| f).collect();
                            let named: Vec<String> = pending.iter().filter(|p| named_in(p, &done)).cloned().collect();
                            let src = if named.is_empty() { &pending } else { &named };
                            for p in src {
                                if !fixed.contains(p) {
                                    fixed.push(p.clone());
                                }
                            }
                            let mut named_here: Vec<String> = Vec::new();
                            for f in &done {
                                for t in failed_tests(f) {
                                    if !named_here.iter().any(|x| x == t) {
                                        named_here.push(t.to_string());
                                    }
                                }
                            }
                            for t in &named_here {
                                if !tests.contains(t) {
                                    tests.push(t.clone());
                                }
                            }
                            for f in &done {
                                unnamed = unnamed.max(failure_count(f) - named_here.len() as i64);
                            }
                            if failures.is_empty() {
                                pending.clear();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut open: Vec<String> = Vec::new();
        if let Some((_, f)) = failures.last() {
            for t in failed_tests(f) {
                open.push(t.to_string());
            }
        }
        open.sort();
        open.dedup();
        if changed.is_empty() && tests.is_empty() && open.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if !changed.is_empty() {
            out = format!("\n  files changed: {}", changed[..changed.len().min(12)].join(", "));
            if changed.len() > 12 {
                out += " …";
            }
        }
        if !fixed.is_empty() {
            out +=
                &format!("; the changes that made the failing tests pass: {}", fixed[..fixed.len().min(8)].join(", "));
        }
        if !tests.is_empty() {
            out += if out.is_empty() { "\n  " } else { "; " };
            let mut s = tests.clone();
            s.sort();
            out += &format!("the tests that failed and now pass: {}", s[..s.len().min(16)].join(", "));
            if tests.len() > 16 {
                out += " …";
            }
            if unnamed > 0 {
                out += &format!(" (the runs counted {unnamed} more failing, not named in what was shown)");
            }
        }
        if !open.is_empty() {
            out += if out.is_empty() { "\n  " } else { "; " };
            out += &format!("tests failing in the last run in them: {}", open[..open.len().min(16)].join(", "));
            if open.len() > 16 {
                out += " …";
            }
        }
        out
    }

    /// Everything the engine holds, by zone, and what of it the next call takes.
    pub fn memory(&mut self) -> Value {
        let n = self.tr.msgs.len();
        let mut zone = vec![1usize; n];
        let mut zones = vec![json!({"label": "anchor", "active": true, "card": false})];
        let g = &self.goal;
        if !g.turns.is_empty() {
            let a = g.active();
            let others = g.others();
            let shown = if self.apart() { head_slice(others.len(), self.p.cards) } else { 0 };
            let carded: FxHashSet<usize> = others[..shown].iter().copied().collect();
            for (k, members) in g.members.iter().enumerate() {
                zones.push(json!({"label": g.ask(&self.tr, &self.fz, members[0]), "active": k == a || shown == 0 && !self.apart(), "card": carded.contains(&k)}));
                for &j in members {
                    let (x, y) = g.span(j, n);
                    for z in &mut zone[x..y] {
                        *z = k + 1;
                    }
                }
            }
        } else {
            zones.push(json!({"label": "conversation", "active": true, "card": false}));
        }
        if n > 0 {
            zone[0] = 0;
        }
        let seen: FxHashSet<usize> = self.visible().iter().copied().collect();
        let cells: Vec<Value> = (0..n)
            .map(|mi| {
                let state = if seen.contains(&mi) {
                    if self.fz.by_msg(mi).iter().any(|i| self.dropped.contains_key(i)) {
                        "cut"
                    } else {
                        "whole"
                    }
                } else {
                    "out"
                };
                json!({"zone": zone[mi], "tokens": self.sizes[mi].max(1), "state": state})
            })
            .collect();
        json!({"zones": zones, "cells": cells})
    }
}

/// The form of a name: each run of letters and digits that holds a digit becomes `#`, so `loc_3D60` and
/// `loc_2A10` share `loc_#`. None when no run is free of digits (a bare number or address).
fn form(w: &str) -> Option<String> {
    let (mut out, mut run, mut named) = (String::new(), String::new(), false);
    for c in w.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() {
            run.push(c);
            continue;
        }
        if run.chars().any(|c| c.is_ascii_digit()) {
            out.push('#');
        } else if !run.is_empty() {
            named = true;
            out.push_str(&run);
        }
        run.clear();
        out.push(c);
    }
    out.pop();
    named.then_some(out)
}

fn command_of(b: &Value) -> String {
    match b.get("input") {
        Some(Value::Object(o)) => o.get("command").map_or(String::new(), py_str),
        _ => String::new(),
    }
}

/// The message (from 0) a `recall` call names: `m00042`, `m42` or `42` is the 42nd.
fn message_named(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches(['m', 'M', '#']);
    n.parse::<usize>().ok().filter(|&k| k > 0).map(|k| k - 1)
}

/// The topic words of a text: its words of three letters or more, in lower case, digits apart, and its
/// single capital letters, which label things ("function B", "table C", "assembly-A.txt"). Unlike the
/// distinctive words, which show what a reply copied, these show what a text is about ("helm",
/// "invoice", "boost"); how rare each is in the session (`asks`) tells which point anywhere.
fn topics(text: &str, int: &mut Interner) -> FxHashSet<Sym> {
    let mut out = FxHashSet::default();
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        if w.len() == 1 && w.chars().all(|c| c.is_ascii_uppercase()) {
            out.insert(int.intern(w));
        } else if w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) {
            out.insert(int.intern(&w.to_lowercase()));
        }
    }
    out
}

/// The steps of a request's work: each agent message with the tool results that answer it. A step that
/// only writes notes is left out: the notes are what the agent took from the others, and they stay.
fn steps_of(tr: &Tracer, work: Vec<usize>) -> Vec<Vec<usize>> {
    let mut steps: Vec<Vec<usize>> = Vec::new();
    for mi in work {
        let agent = tr.msgs[mi].get("role").and_then(Value::as_str) == Some("assistant");
        match steps.last_mut() {
            Some(s) if !agent => s.push(mi),
            _ => steps.push(vec![mi]),
        }
    }
    steps.retain(|s| {
        let calls: Vec<String> = blocks(tr.msgs[s[0]].get("content"))
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .map(|b| b.get("name").and_then(Value::as_str).unwrap_or("").to_string())
            .collect();
        calls.is_empty() || calls.iter().any(|c| c != NOTE_TOOL)
    });
    steps
}

fn id_key(v: Option<&Value>) -> String {
    match v {
        None => key_of(Some(&Value::String(String::new()))),
        Some(v) => key_of(Some(v)),
    }
}

fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn failure_count(f: &str) -> i64 {
    FAILURE_COUNT
        .captures_all(f)
        .iter()
        .map(|c| {
            let g = c.get(1).copied().flatten().or_else(|| c.get(2).copied().flatten());
            g.and_then(|(a, b)| f[a..b].parse::<i64>().ok()).unwrap_or(0)
        })
        .sum()
}

/// Whether a passing run of ``pass`` speaks for the tests a failing run of ``failed`` ran: the same command, or
/// a broader one (``failed`` is ``pass`` and more arguments).
fn covers(pass: &str, failed: &str) -> bool {
    failed.strip_prefix(pass).is_some_and(|r| r.is_empty() || r.starts_with(' '))
}

/// The test run a shell command makes: its first piece (between `&&`, `||`, `;`, `|`) that runs tests, without
/// redirections, so `cd app && npm test 2>&1 | tail -5` and `npm test` are one run.
fn test_run(command: &str) -> String {
    let joined = command.replace("&&", "\n").replace("||", "\n");
    let Some(piece) = joined.split(['\n', ';', '|']).find(|s| TEST_COMMAND.is_match(s)) else {
        return command.trim().to_string();
    };
    let mut out: Vec<&str> = Vec::new();
    let mut skip = false;
    for t in py::split_ws(piece) {
        let bare = t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if std::mem::take(&mut skip) {
            continue;
        }
        if bare.starts_with(['<', '>']) {
            // `2>&1`, `>out.txt`; a bare `>` takes the next word as its file
            skip = bare.trim_end_matches('&').chars().all(|c| c == '<' || c == '>');
            continue;
        }
        out.push(t);
    }
    out.join(" ")
}

fn named_in(path: &str, failures: &[String]) -> bool {
    let base = basename(path);
    let stem = base.split('.').next().unwrap_or("");
    if stem.is_empty() || TEST_NAME.is_match(stem) {
        return false;
    }
    let pat = format!(r"(?:\btests?_|\b){}(?:_tests?|[._](?:test|spec))?\.\w+\b", py::escape(stem));
    let Ok(tested) = Re::try_new(&pat, 0) else { return false };
    failures.iter().any(|f| f.contains(path) || tested.find_all(f).iter().any(|&(a, b)| TEST_NAME.is_match(&f[a..b])))
}

/// The parts of a text's words (split at anything not a letter or a digit): long ones in lower case, the
/// short ones (one or two characters) as written, where case is all that tells a name from a word.
fn name_parts(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| if p.chars().count() > 2 { p.to_lowercase() } else { p.to_string() })
        .collect()
}

/// The files a call names, each as the parts of its name without the extension (`material/assembly-B.txt`
/// → `assembly`, `B`). A file is a word whose last part has a short extension (`.txt`, `.rs`, `.json`) and
/// that is not an option or a pattern: a folder, a range or a regex names no file.
fn files_in(call: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for w in call.split(|c: char| c.is_whitespace() || ";|&<>'\"`(),=".contains(c)) {
        // a drive in front (`C:\`, `C:/`) is no part of the name
        let b = w.as_bytes();
        let w = if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
            &w[2..]
        } else {
            w
        };
        if w.starts_with('-') || (w.contains('\\') && !windows_path(w)) || w.contains(['*', '?', '$', ':']) {
            continue;
        }
        let base = basename(w);
        let stem = match base.rsplit_once('.') {
            Some((s, ext))
                if !s.is_empty() && (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                s
            }
            _ => continue,
        };
        let parts = name_parts(stem);
        if !parts.is_empty() && parts.iter().any(|p| p.chars().any(|c| c.is_alphabetic())) && !out.contains(&parts) {
            out.push(parts);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings of the mechanism tests: each test turns on what it checks over this base, so a change of
    /// the default spec does not move them.
    const BASE: &str = "directives=1;nudge=8;close=0;ahead=0;dedup=0;live=0;lead=0;aim=0;follow=0;brief=0;digest=0;hold=0;loops=0;asked=0;sup=0;named=0;margin=1;mission=0";

    fn params(spec: &str) -> Result<Params, String> {
        crate::params(&format!("{BASE};{spec}"))
    }

    /// A step of the agent: a shell command and its output.
    fn step(k: usize, command: &str, out: &str) -> [Value; 2] {
        let id = format!("call_{k}");
        [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "bash", "input": {"command": command}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out}]}),
        ]
    }

    /// A made-up word, different for every seed.
    fn word(seed: usize) -> String {
        const SYL: [&str; 16] =
            ["ka", "lo", "mi", "ren", "tu", "vas", "zel", "or", "pim", "qu", "dra", "sen", "ul", "fo", "bri", "hax"];
        let mut x = (seed as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut w = String::new();
        for _ in 0..3 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            w += SYL[(x >> 60) as usize];
        }
        w
    }

    /// Lines `a..=b` of a listing, no two of the same shape (a squeeze folds lines that differ only in
    /// their numbers).
    fn listing(a: usize, b: usize) -> String {
        (a..=b)
            .map(|l| format!("{:>5}:  {} {}, {}   ; slot {l}", 0x400 + 4 * l, word(l), word(l + 100), word(l + 200)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// An engine with `spec` that took in a request and reads of `ranges` of the listing.
    fn reads(spec: &str, ranges: &[(usize, usize)]) -> Engine {
        let mut e = Engine::new(params(&format!("fresh=0;{spec}")).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for (k, &(a, b)) in ranges.iter().enumerate() {
            msgs.extend(step(k + 1, &format!("sed -n '{a},{b}p' listing.txt"), &listing(a, b)));
        }
        e.sync(&msgs);
        e
    }

    /// The text of the tool result in message `mi` of the view.
    fn output(view: &[Value], mi: usize) -> String {
        text_of(blocks(view[mi].get("content"))[0].get("content")).into_owned()
    }

    #[test]
    fn a_request_still_open_after_n_2n_4n_steps_comes_again_in_its_own_words_and_stays_where_it_went() {
        let ask = "Read listing.txt and say what slot 50 holds.";
        let mut msgs = vec![json!({"role": "user", "content": ask})];
        for k in 0..20 {
            msgs.extend(step(
                k + 1,
                &format!("sed -n '{},{}p' listing.txt", 10 * k + 1, 10 * k + 10),
                &listing(10 * k + 1, 10 * k + 10),
            ));
        }
        let lines = |spec: &str| {
            let mut e = Engine::new(params(spec).unwrap());
            let mut seen: Vec<(usize, String)> = Vec::new();
            for t in (3..=msgs.len()).step_by(2) {
                e.sync(&msgs[..t]);
                let view = e.view();
                for (mi, m) in view.iter().enumerate() {
                    let said = blocks(m.get("content"))
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .filter(|s| s.contains("The request this work is for"))
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    if let Some(s) = said.first() {
                        // once in, the line stays as it went: the cache keeps the message
                        match seen.iter().find(|(at, _)| *at == mi) {
                            Some((_, before)) => assert_eq!(before, s),
                            None => seen.push((mi, s.clone())),
                        }
                    }
                }
            }
            seen
        };
        let seen = lines("nudge=0;recite=4");
        // steps 4, 8 and 16 of 20: each on the output of that step (message 2k)
        assert_eq!(seen.iter().map(|(mi, _)| *mi).collect::<Vec<_>>(), vec![8, 16, 32]);
        assert!(seen.iter().all(|(_, s)| s.ends_with(ask)), "{seen:?}");
        assert!(lines("nudge=0").is_empty());
    }

    #[test]
    fn a_call_run_again_says_how_often_it_ran_and_whether_its_output_moved() {
        let mut msgs = vec![json!({"role": "user", "content": "Find why the counter in status.txt is stuck."})];
        let outs = [listing(1, 12), listing(1, 12), listing(1, 12), listing(5, 16), listing(1, 12)];
        for (k, out) in outs.iter().enumerate() {
            msgs.extend(step(k + 1, "cat status.txt", out));
        }
        let said = |spec: &str| {
            let mut e = Engine::new(params(spec).unwrap());
            e.sync(&msgs);
            e.view()
                .iter()
                .flat_map(|m| blocks(m.get("content")).into_owned())
                .filter_map(|b| {
                    let t = b.get("text").and_then(Value::as_str).map(str::to_string).or_else(|| {
                        blocks(b.get("content"))
                            .iter()
                            .find_map(|c| c.get("text").and_then(Value::as_str))
                            .map(str::to_string)
                    })?;
                    t.contains(" times").then_some(t)
                })
                .collect::<Vec<_>>()
        };
        let lines = said("nudge=0;dedup=3;asked=3;reruns=1");
        let has = |s: &str| lines.iter().any(|l| l.contains(s));
        assert!(has("ran 2 times in this session: the same output each time"), "{lines:?}");
        assert!(has("ran 3 times in this session: the same output each time"), "{lines:?}");
        assert!(has("ran 4 times in this session: its output changed 1 of those times, this time too"), "{lines:?}");
        assert!(has("ran 5 times in this session: its output changed 2 of those times, this time too"), "{lines:?}");
        assert!(said("nudge=0;dedup=3;asked=3").is_empty());
    }

    /// What `fixes` says of a session that edits `src/calc.rs` between test runs, each run a command
    /// and its output.
    fn fixes_of(runs: &[(&str, &str)]) -> String {
        let mut msgs = vec![json!({"role": "user", "content": "Make the test suite pass."})];
        for (k, (cmd, out)) in runs.iter().enumerate() {
            msgs.extend(step(2 * k + 1, cmd, out));
            msgs.extend(step(2 * k + 2, &format!("sed -i 's/a{k}/b{k}/' src/calc.rs"), ""));
        }
        let mut e = Engine::new(params("nudge=0").unwrap());
        e.sync(&msgs);
        e.fixes(0, msgs.len(), Some(msgs.len()))
    }

    const FAILING_RUN: &str = "running 2 tests\ntest calc::test_round ... FAILED\nfailures:\n    calc::test_round\ntest result: FAILED. 1 passed; 1 failed";
    const PASSING_RUN: &str = "running 2 tests\ntest result: ok. 2 passed; 0 failed";

    #[test]
    fn narrower_passing_run_does_not_mark_earlier_failures_fixed() {
        for (broad, narrow) in [
            ("cargo test", "cargo test unrelated"),
            ("npm test", "npm test -- -t other"),
            ("pytest", "pytest tests/b.py"),
        ] {
            let said = fixes_of(&[(broad, FAILING_RUN), (narrow, PASSING_RUN)]);
            assert!(!said.contains("now pass"), "{narrow}: {said}");
            assert!(!said.contains("made the failing tests pass"), "{narrow}: {said}");
            assert!(said.contains("tests failing in the last run in them: test_round"), "{narrow}: {said}");
        }
        // the same command passing, or a broader one, still marks them fixed
        for (failing, passing) in [
            ("cargo test", "cargo test"),
            ("cargo test calc", "cargo test"),
            ("cd app && npm test 2>&1 | tail -40", "npm test > out.txt; cat out.txt"),
        ] {
            let said = fixes_of(&[(failing, FAILING_RUN), (passing, PASSING_RUN)]);
            assert!(said.contains("the tests that failed and now pass: test_round"), "{said}");
            assert!(said.contains("the changes that made the failing tests pass: src/calc.rs"), "{said}");
            assert!(!said.contains("failing in the last run"), "{said}");
        }
    }

    #[test]
    fn test_that_fails_again_is_not_listed_as_now_passing() {
        let said = fixes_of(&[("cargo test", FAILING_RUN), ("cargo test", PASSING_RUN), ("cargo test", FAILING_RUN)]);
        assert!(!said.contains("now pass"), "{said}");
        assert!(said.contains("tests failing in the last run in them: test_round"), "{said}");
    }

    /// Failing runs of several ecosystems, each piped so that the command exits 0 (no `is_error`), with the
    /// test the run names (empty: the output names none in a shape the curator reads).
    const FAILING_RUNS: [(&str, &str, &str); 12] = [
        ("go test ./... 2>&1 | tail -20", "--- FAIL: TestAdd (0.00s)\n    calc_test.go:9: got 3, want 4\nFAIL\nexit status 1\nFAIL\texample/calc\t0.002s", "TestAdd"),
        ("npx mocha | tail -5", "  2 passing (8ms)\n  1 failing\n\n  1) Cart\n       adds item:\n     expected 3 to equal 4", ""),
        ("bundle exec rspec | tail -8", "Finished in 0.01 seconds\n3 examples, 1 failure\n\nFailed examples:\n\nrspec ./spec/cart_spec.rb:12 # Cart adds item", "./spec/cart_spec.rb:12"),
        ("npx jest 2>&1 | tail -9", "FAIL src/cart.test.js\n  ● Cart › adds item\n\n    expect(received).toBe(expected)\n\nTests:       1 failed, 2 passed, 3 total", "Cart › adds item"),
        ("cargo test 2>&1 | tail -6", "test parser::parses_empty ... FAILED\n\nfailures:\n    parser::parses_empty\n\ntest result: FAILED. 4 passed; 1 failed", "parses_empty"),
        ("./gradlew test | tail -4", "CalcTest > testAdd() FAILED\n    org.opentest4j.AssertionFailedError at CalcTest.java:9\n3 tests completed, 1 failed", "testAdd"),
        ("mvn test | tail -4", "Tests run: 3, Failures: 1, Errors: 0, Skipped: 0\n[INFO] BUILD FAILURE", ""),
        ("vendor/bin/phpunit | tail -3", "FAILURES!\nTests: 3, Assertions: 3, Failures: 1.", ""),
        ("dotnet test | tail -2", "Failed!  - Failed:     1, Passed:     2, Skipped:     0, Total:     3", ""),
        ("mix test | tail -2", "Finished in 0.03 seconds\n3 tests, 1 failure", ""),
        ("ctest | tail -2", "67% tests passed, 1 tests failed out of 3", ""),
        ("node --test | tail -3", "not ok 2 - adds item\n# pass 2\n# fail 1", "adds item"),
    ];
    /// The passing output of each of those commands, in the same order.
    const PASSING_RUNS: [&str; 12] = [
        "ok  \texample/calc\t0.002s",
        "  3 passing (8ms)",
        "Finished in 0.01 seconds\n3 examples, 0 failures",
        "PASS src/cart.test.js\nTests:       3 passed, 3 total",
        "test result: ok. 5 passed; 0 failed",
        "BUILD SUCCESSFUL in 2s",
        "Tests run: 3, Failures: 0, Errors: 0, Skipped: 0\n[INFO] BUILD SUCCESS",
        "OK (3 tests, 3 assertions)",
        "Passed!  - Failed:     0, Passed:     3, Skipped:     0, Total:     3",
        "3 tests, 0 failures",
        "100% tests passed, 0 tests failed out of 3",
        "# pass 3\n# fail 0",
    ];

    #[test]
    fn failing_runs_of_any_ecosystem_are_not_read_as_passing() {
        for ((cmd, out, name), pass) in FAILING_RUNS.iter().zip(PASSING_RUNS) {
            // a failing run, an edit, the same failure again: nothing passes
            let said = fixes_of(&[(cmd, out), (cmd, out)]);
            assert!(!said.contains("now pass"), "{cmd}: {said}");
            assert!(!said.contains("made the failing tests pass"), "{cmd}: {said}");
            // a passing run after it does mark it fixed, with the test it named
            let said = fixes_of(&[(cmd, out), (cmd, pass)]);
            assert!(said.contains("the changes that made the failing tests pass: src/calc.rs"), "{cmd}: {said}");
            if !name.is_empty() {
                assert!(said.contains(&format!("the tests that failed and now pass: {name}")), "{cmd}: {said}");
                let open = fixes_of(&[(cmd, out)]);
                assert!(open.contains(&format!("tests failing in the last run in them: {name}")), "{cmd}: {open}");
            }
        }
        // a count the run's own script prints next to another number is a label, not a failure
        let said = fixes_of(&[("pytest", FAILING_RUN), ("pytest; ./check", "3 passed\nrows 20 failures 0\nerrors: 0")]);
        assert!(said.contains("the tests that failed and now pass: test_round"), "{said}");
    }

    #[test]
    fn passing_runs_that_name_errors_or_exit_codes_are_passing() {
        // test names, package paths and logs of a passing run that hold a failure word and a number
        let passing = [
            "ok  \tgithub.com/acme/api/errors\t1.204s\nok  \tgithub.com/acme/api/fail\t0.3s",
            "PASS src/api.test.js\n  ✓ returns error 404 for an unknown id (5 ms)\n  ✓ returns error 422\nTests:       2 passed, 2 total",
            "  ✓ exits with exit code 2 on bad args\n  ✓ prints usage on exit code 1\n  2 passing (4ms)",
            "  returns error 404 for an unknown id\n  2 passing (9ms)",
            "INFO HTTP error 503, retrying\nWARN upstream error: 502 from cache\n3 passed in 0.2s",
            "Cart\n  reports failed 2 items as skipped\n2 examples, 0 failures",
            "  ✓ rejects when the worker exited with code 1 (3 ms)\nTests:       4 passed, 4 total",
            "✔ reports a build that failed with exit code 2 (1.2ms)\nℹ tests 1\nℹ pass 1\nℹ fail 0",
            "ok 3 - process exited with code 1 is shown\nPASS job finished with exit code 1 is retried",
            "[info] Passed: Total 2, Failed 0, Errors 0, Passed 2",
            "Tests: succeeded 2, failed 0, canceled 0, ignored 0, pending 0",
        ];
        for out in passing {
            assert!(!TEST_FAILED.is_match(out), "{out}");
            assert!(!ERROR_MARKS.is_match(out), "{out}");
        }
        let said = fixes_of(&[(FAILING_RUNS[0].0, FAILING_RUNS[0].1), (FAILING_RUNS[0].0, passing[0])]);
        assert!(said.contains("the tests that failed and now pass: TestAdd"), "{said}");
        let said = fixes_of(&[("npx jest", FAILING_RUNS[3].1), ("npx jest", passing[1])]);
        assert!(said.contains("the tests that failed and now pass: Cart › adds item"), "{said}");
        // failure counts and exit codes as runners and hosts print them still read as failing
        for out in FAILING_RUNS.iter().map(|r| r.1).chain([
            FAILING_RUN,
            "Tests run: 3, Failures: 1, Errors: 0",
            "Tests: 3, Assertions: 3, Failures: 1.",
            "Test summary: total: 3, failed: 1, succeeded: 2",
            "Total tests: 3\n     Passed: 2\n     Failed: 1",
            "Ran 3 tests in 0.01s\n\n(failures=1)",
            "Tests failed: 1, passed: 2",
            "# pass 2\n# fail 1",
            "ℹ tests 3\nℹ pass 2\nℹ fail 1",
            "✖ adds item (2.1ms)\n  AssertionError: expected 3 to equal 4",
            "[error] Failed: Total 2, Failed 1, Errors 0, Passed 1",
            "Tests: succeeded 1, failed 1, canceled 0, ignored 0, pending 0",
            "ok 1 - a\n\n[exit code 1]",
            "Exit code: 2\nWall time: 1.2 seconds",
            "error Command failed with exit code 1.",
            "make: *** [Makefile:4: test] Error 2",
            "exit status 1",
        ]) {
            assert!(TEST_FAILED.is_match(out), "{out}");
        }
    }

    #[test]
    fn test_commands_of_common_runners_are_test_runs() {
        for cmd in [
            "./gradlew test",
            "gradle test",
            "python manage.py test",
            "bin/rails test",
            "bundle exec rake spec",
            "flutter test",
            "php artisan test",
            "npx playwright test",
            "cargo nextest run",
            "node --test",
            "sbt test",
            "bazel test //...",
        ] {
            assert!(TEST_COMMAND.is_match(cmd), "{cmd}");
        }
        for cmd in ["cat test.txt", "ls tests", "git commit -m 'test'", "echo test"] {
            assert!(!TEST_COMMAND.is_match(cmd), "{cmd}");
        }
    }

    #[test]
    fn only_a_read_of_a_path_weighs_as_on_disk_in_the_ledger() {
        let mut msgs = vec![json!({"role": "user", "content": "Map where each module of the realm is configured."})];
        for k in 0..40 {
            let out = format!("the module_{k} service reads its limits from the shared table and retries them");
            // a file read, and a read of a thread (a tool with no path): only the file is one read away
            let input = if k % 2 == 0 {
                json!({"path": format!("config/{}.toml", word(k))})
            } else {
                json!({"thread": word(k)})
            };
            let name = if k % 2 == 0 { "read_file" } else { "read_thread" };
            msgs.extend(tool(k + 1, name, input, &out, false));
            msgs.push(json!({"role": "user", "content": "Go on."}));
        }
        let mut e = Engine::new(params("ledger=6000;cap=3000;eoq=0;nudge=0").unwrap());
        e.sync(&msgs);
        e.view();
        assert!(e.start > 0 && !e.pool.is_empty());
        let weight = |k: usize| e.pool.iter().find(|x| x.1.contains(&format!("module_{k} "))).map(|x| x.3);
        let (file, thread) = ((0..40).step_by(2).find_map(weight), (1..40).step_by(2).find_map(weight));
        assert_eq!(file, Some(ON_DISK));
        assert_eq!(thread, Some(1.0));
    }

    #[test]
    fn ledger_text_fits_its_budget_with_header() {
        let mut msgs = vec![json!({"role": "user", "content": "Map where each module of the realm is configured."})];
        for k in 0..40 {
            let out: String = (0..6)
                .map(|j| format!("{} = {} in config/{}.toml\n", word(k * 10 + j), word(k * 10 + j + 900), word(k)))
                .collect();
            msgs.extend(step(k + 1, &format!("cat config/{}.toml", word(k)), &out));
            msgs.push(
                json!({"role": "assistant", "content": format!("{} is set in config/{}.toml.", word(k * 10), word(k))}),
            );
            msgs.push(json!({"role": "user", "content": "Go on."}));
        }
        let mut e = Engine::new(params("ledger=6000;cap=3000;eoq=0;nudge=0").unwrap());
        e.sync(&msgs);
        e.view();
        assert!(e.start > 0 && !e.pool.is_empty());
        let mut some = false;
        for budget in [40, 60, 100, 200, 400] {
            let text = e.ledger_text(budget);
            some |= !text.is_empty();
            assert!(est_str(&text) <= budget, "{budget}: {} tokens\n{text}", est_str(&text));
        }
        assert!(some);
    }

    #[test]
    fn with_stall_the_request_comes_again_only_once_the_outputs_stop_bringing_new_lines() {
        let ask = "Read listing.txt and say what slot 50 holds.";
        // reads that keep going over lines 1-12, and reads that move on to new lines each time
        let round = |k: usize| listing(1 + k % 2, 12 + k % 2);
        let ahead = |k: usize| listing(12 * k + 1, 12 * k + 12);
        let said = |out: &dyn Fn(usize) -> String| {
            let mut msgs = vec![json!({"role": "user", "content": ask})];
            for k in 0..14 {
                msgs.extend(step(k + 1, &format!("sed -n '{k}p' listing.txt"), &out(k)));
            }
            let mut e = Engine::new(params("nudge=0;dedup=3;stall=4").unwrap());
            let mut lines = Vec::new();
            for t in (3..=msgs.len()).step_by(2) {
                e.sync(&msgs[..t]);
                let view = e.view();
                let last = blocks(view[view.len() - 1].get("content")).into_owned();
                lines.extend(
                    last.iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .filter(|s| s.contains("The request this work is for"))
                        .map(str::to_string),
                );
            }
            lines
        };
        let going_round = said(&round);
        assert!(!going_round.is_empty());
        assert!(going_round[0].contains("were new.") && going_round[0].ends_with(ask), "{going_round:?}");
        assert!(said(&ahead).is_empty());
    }

    #[test]
    fn with_task_the_persons_words_on_the_results_of_a_step_open_a_new_request() {
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for k in 0..3 {
            msgs.extend(step(
                k + 1,
                &format!("sed -n '{},{}p' listing.txt", 10 * k + 1, 10 * k + 10),
                &listing(10 * k + 1, 10 * k + 10),
            ));
        }
        let last = msgs.len() - 1;
        let mut bs = blocks(msgs[last].get("content")).into_owned();
        bs.push(json!({"type": "text", "text": "Now say what slot 7 holds instead."}));
        msgs[last]["content"] = Value::Array(bs);
        let turns = |spec: &str| {
            let mut e = Engine::new(params(spec).unwrap());
            e.sync(&msgs);
            e.goal.turns.iter().map(|t| t.start).collect::<Vec<_>>()
        };
        assert_eq!(turns("nudge=0"), vec![0]);
        assert_eq!(turns("nudge=0;task=1"), vec![0, last]);
    }

    #[test]
    fn a_request_that_rides_on_the_results_of_a_step_is_the_one_sent_again() {
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for k in 0..4 {
            msgs.extend(step(
                k + 1,
                &format!("sed -n '{},{}p' listing.txt", 10 * k + 1, 10 * k + 10),
                &listing(10 * k + 1, 10 * k + 10),
            ));
        }
        // the next request comes with the results of the last step, as a host or a person adds it
        let next = "Now say what slot 7 holds instead.";
        let last = msgs.len() - 1;
        let mut bs = blocks(msgs[last].get("content")).into_owned();
        bs.push(json!({"type": "text", "text": next}));
        msgs[last]["content"] = Value::Array(bs);
        for k in 4..9 {
            msgs.extend(step(
                k + 1,
                &format!("sed -n '{},{}p' listing.txt", 10 * k + 1, 10 * k + 10),
                &listing(10 * k + 1, 10 * k + 10),
            ));
        }
        let mut e = Engine::new(params("nudge=0;recite=4").unwrap());
        e.sync(&msgs);
        let said: Vec<String> = e
            .view()
            .iter()
            .flat_map(|m| blocks(m.get("content")).into_owned())
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .filter(|s| s.contains("The request this work is for"))
            .collect();
        assert!(!said.is_empty());
        assert!(said.last().unwrap().ends_with(next), "{said:?}");
    }

    #[test]
    fn what_a_command_prints_of_a_file_comes_whole_and_the_output_of_a_process_folds() {
        // lines of one form whose numbers count up: the output of a process folds them, a file holds them
        let rows = (0..60).map(|k| format!("movdqa xmm0, cs:xmmword_{}", 4340 + 10 * k)).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "What does the listing load?"})];
        msgs.extend(step(1, "cd material; sed -n 1,60p listing.txt", &rows));
        msgs.extend(step(2, "./gen.sh", &rows));
        let mut e = Engine::new(params("nudge=0").unwrap());
        // with a way back (the host offers `recall`) the output of a process folds with a loss
        e.tools(&[RECALL_TOOL.to_string()]);
        e.sync(&msgs);
        let view = e.view();
        assert_eq!(output(&view, 2), rows);
        assert!(output(&view, 4).contains("more lines with the same pattern"), "{view:?}");
        // without one it folds only what comes back whole: every value is still there
        let mut e = Engine::new(params("nudge=0").unwrap());
        e.sync(&msgs);
        let view = e.view();
        let out = output(&view, 4);
        assert!(!out.contains("more lines with the same pattern"), "{out}");
        assert!((0..60).all(|k| out.contains(&(4340 + 10 * k).to_string())), "{out}");
    }

    #[test]
    fn past_the_cap_an_old_copy_leaves_before_the_first_file_read() {
        // the file read first, then the same log read twice, then short steps
        let spec =
            (0..200).map(|k| format!("field_{k} = {} {}", word(k), word(k + 1000))).collect::<Vec<_>>().join("\n");
        let log =
            (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "What does the spec say the log should hold?"})];
        msgs.extend(step(1, "cat spec.txt", &spec));
        msgs.extend(step(2, "cat run.log", &log));
        msgs.extend(step(3, "cat run.log", &log));
        for k in 4..11 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let results = |view: &[Value]| -> Vec<String> {
            view.iter()
                .flat_map(|m| blocks(m.get("content")).into_owned())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| text_of(b.get("content")).into_owned())
                .collect()
        };
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        // a cap one token short, and a floor that dropping one copy of the log is enough for
        let cap = total - 1;
        let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
        let given = format!("threshold=0;nudge=0;cap={cap};floor={floor}");
        let mut age = Engine::new(params(&given).unwrap());
        age.sync(&msgs);
        assert!(!results(&age.view()).contains(&spec), "the oldest messages leave first");
        // the rewrite counts what the cache misses from the first message it changed on
        let miss = |k: &str| age.stats.get(k).copied().unwrap_or(0);
        assert!(miss("miss_cap") > 0 && miss("miss") >= miss("miss_cap"), "{:?}", age.stats);
        let mut need = Engine::new(params(&format!("{given};cut=need")).unwrap());
        need.sync(&msgs);
        let kept = results(&need.view());
        assert!(kept.contains(&spec), "{kept:?}");
        assert_eq!(kept.iter().filter(|r| **r == log).count(), 1, "{kept:?}");
        // only parts a later output holds went
        assert!(need.stats.get("need_copies").is_some_and(|n| *n > 0) && !need.stats.contains_key("need_chunks"));
    }

    #[test]
    fn a_long_paste_among_the_latest_messages_is_not_cut_around_on_every_request() {
        // a request answered in ten small steps, then a long log pasted into the next request: while the log
        // is among the protected latest messages no cut can take it, and cutting what is before it a little
        // on every request would make the provider read the whole view again each time
        let mut msgs = vec![json!({"role": "user", "content": "List the slots of listing.txt one range at a time."})];
        for k in 0..10 {
            msgs.extend(step(k, &format!("sed -n {k}p listing.txt"), &listing(10 * k, 10 * k + 9)));
        }
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done: 100 slots listed."}]}));
        let log: String = (0..1500)
            .map(|k| format!("run {k} failed at {} with {}", word(k + 5000), word(k + 9000)))
            .collect::<Vec<_>>()
            .join("\n");
        msgs.push(json!({"role": "user", "content": format!("The run failed, here is its log:\n{log}")}));
        let paste = msgs.len() - 1;
        for k in 10..20 {
            msgs.extend(step(k, &format!("grep -n {} run.log", word(k + 5000)), &format!("{k}: run {k} failed")));
        }
        let mut e = Engine::new(params("nudge=0;eoq=2000;protect=12").unwrap());
        let mut seen = Vec::new();
        for n in (paste + 1..=msgs.len()).step_by(2) {
            e.sync(&msgs[..n]);
            let view = sent(&e.view());
            seen.push((n, e.stats.get("miss").copied().unwrap_or(0), view.contains(log.lines().nth(700).unwrap())));
        }
        let (held, after): (Vec<&(usize, i64, bool)>, Vec<_>) = seen.iter().partition(|s| s.0 <= paste + 12);
        assert!(held.iter().all(|s| s.1 == held[0].1 && s.2), "the view grows as sent: {seen:?}");
        assert!(e.stats.get("cap_stuck").is_some_and(|n| *n > 0), "{:?}", e.stats);
        // once the log is out of the tail a cut makes room, and the view is cut
        assert!(after.last().is_some_and(|s| s.1 > held[0].1), "{seen:?}");
    }

    #[test]
    fn the_output_that_sends_the_lines_of_a_copy_left_out_stays_in_that_pass_and_the_next() {
        let log =
            (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "What does the log hold?"})];
        msgs.extend(step(1, "cat run.log", &log));
        msgs.extend(step(2, "cat run.log", &log));
        for k in 3..8 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        // how many of the log's distinct lines the view sends, and whether it says a later output sends a copy's lines
        let seen = |view: &[Value]| -> (usize, bool) {
            let text: String = view.iter().map(|m| crate::msg::flat(m.get("content")) + "\n").collect();
            let lines: FxHashSet<&str> = text.lines().filter(|l| l.starts_with("event ")).collect();
            (lines.len(), text.contains("a later output below sends"))
        };
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        // a cap one token short, a floor that leaving one copy of the log is enough for, and every other part
        // a guess would let go (`threshold=1`): only the stub of the copy says the log is still sent
        let cap = total - 1;
        let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
        for cut in ["age", "need", "copies", "cover"] {
            let given = format!("threshold=1;still=0;protect=2;nudge=0;sup=1;cap={cap};floor={floor};cut={cut}");
            let mut e = Engine::new(params(&given).unwrap());
            e.sync(&msgs);
            let (lines, stub) = seen(&e.view());
            assert!(stub && !e.copied.is_empty(), "{cut}: {:?}", e.stats);
            assert_eq!(lines, 200, "{cut}: {:?}", e.stats);
            // the next requests weigh what to leave again: the output that sends the copy's lines stays
            let mut more = msgs.clone();
            for k in 9..12 {
                more.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
                e.sync(&more);
                let (lines, stub) = seen(&e.view());
                assert!(!stub || lines == 200, "{cut}, request {k}: {lines} lines, {:?}", e.stats);
            }
        }
    }

    #[test]
    fn the_reasoning_of_the_latest_reply_stays_with_its_calls_whatever_the_tail() {
        let thought =
            (0..120).map(|k| format!("maybe {} then {}", word(k), word(k + 500))).collect::<Vec<_>>().join("\n");
        let msgs = vec![
            json!({"role": "user", "content": "Find slot 50."}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": thought, "signature": "s"},
                {"type": "tool_use", "id": "call_1", "name": "bash", "input": {"command": "cat listing.txt"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": listing(1, 120)}]}),
        ];
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let cap = whole.tokens() - 1;
        // no tail kept whole and every part a guess: the reasoning of the reply the request goes back to stays
        let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;protect=0;cap={cap};floor=0.5")).unwrap());
        e.sync(&msgs);
        let view = e.view();
        let kept = blocks(view[1].get("content")).iter().any(|b| get_str(b, "thinking") == thought);
        assert!(kept && e.stats.contains_key("cap_rewrites"), "{:?} {:?}", view[1], e.stats);
    }

    #[test]
    fn a_long_argument_left_out_says_the_file_on_disk_is_current_only_for_a_write() {
        let script = (0..40).map(|k| format!("echo {} {}", word(k), word(k + 70))).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "Run the checks."})];
        msgs.extend(step(1, &script, "ok"));
        for k in 2..8 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        // a floor that leaving most of the script is enough for: no message has to leave
        let cap = total - 1;
        let floor = (total - est_str(&script) / 2) as f64 / cap as f64;
        let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;cap={cap};floor={floor}")).unwrap());
        e.sync(&msgs);
        let call = e.view()[1].to_string();
        assert!(call.contains("lines of this argument here are omitted"), "{call} {:?}", e.stats);
        assert!(!call.contains("the file on disk is current"), "{call}");
    }

    #[test]
    fn the_text_an_edit_replaced_is_not_said_to_be_in_the_file() {
        let old = (0..40).map(|k| format!("let {} = {};", word(k), k)).collect::<Vec<_>>().join("\n");
        let new = (0..40).map(|k| format!("let {} = {};", word(k + 70), k)).collect::<Vec<_>>().join("\n");
        let input = json!({"path": "a.rs", "old_string": old, "new_string": new});
        let mut msgs = vec![
            json!({"role": "user", "content": "Rename the bindings."}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "call_1", "name": "edit", "input": input}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "edited a.rs"}]}),
        ];
        for k in 2..8 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        let cap = total - 1;
        let floor = (total - est_str(&old)) as f64 / cap as f64;
        let mut e = Engine::new(params(&format!("threshold=1;still=0;nudge=0;cap={cap};floor={floor}")).unwrap());
        e.sync(&msgs);
        let call = e.view()[1].to_string();
        assert!(call.contains("lines this edit replaced here are omitted; they are no longer in the file"), "{call}");
        assert!(call.contains("lines written here are omitted; the file on disk is current"), "{call}");
    }

    #[test]
    fn what_is_added_to_a_message_that_came_with_no_text_leaves_no_empty_text_block() {
        for content in [json!(""), json!([]), json!([{"type": "text", "text": ""}])] {
            let mut m = json!({"role": "user", "content": content});
            add_blocks(&mut m, [text_block("[curator] the map")]);
            assert_eq!(m["content"], json!([{"type": "text", "text": "[curator] the map"}]));
        }
        let mut m = json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": ""}]});
        add_blocks(&mut m, [text_block("x")]);
        assert_eq!(m["content"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn one_long_line_dense_in_tokens_is_cut_to_its_budget_without_a_panic() {
        // control bytes weigh more than a quarter token each: the budget holds fewer characters than four a token
        let dense = "\u{1}".repeat(100);
        let cut = head_and_tail(&dense, 100);
        assert!(cut.contains("characters cut") && cut.chars().filter(|c| *c == '\u{1}').count() < 100, "{cut:?}");
        // quotes weigh half a token: the head and tail never overlap
        let quotes = "\"".repeat(100);
        let cut = head_and_tail(&quotes, 30);
        assert!(cut.chars().filter(|c| *c == '"').count() < 100, "{cut:?}");
    }

    #[test]
    fn the_anchor_takes_a_rule_the_view_lost_at_once_and_lets_go_of_one_it_holds_again_with_a_rewrite() {
        let mut e = Engine::new(params("").unwrap());
        e.sync(&[json!({"role": "user", "content": "Look at the logs."})]);
        // a rule whose message is not in the view
        e.rules.add(40, "Never delete the logs.".into(), false);
        let v = e.map_ver;
        assert!(e.refresh_rules(false));
        assert!(e.map_ver > v && e.rules_shown.contains("Never delete the logs."), "{}", e.rules_shown);
        // its message is in the view again: the anchor, the first message, stays as it was sent
        e.rules.list[0].msg = Some(0);
        let v = e.map_ver;
        assert!(!e.refresh_rules(false));
        assert_eq!(e.map_ver, v);
        assert!(e.rules_shown.contains("Never delete the logs."));
        // the view is rewritten from its start anyway: the line goes with it
        assert!(e.refresh_rules(true));
        assert!(e.rules_shown.is_empty());
    }

    #[test]
    fn with_nudge_off_an_earlier_request_for_notes_stays_until_the_view_changes() {
        let mut e = Engine::new(params("nudge=0;settle=50;eoq=0;check=100000000;every=100000").unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "cat part1.txt", &listing(1, 60)));
        e.sync(&msgs);
        e.decide();
        assert!(e.nudged.contains(&2), "{:?}", e.nudged);
        msgs.extend(step(2, "cat part2.txt", &listing(61, 120)));
        e.sync(&msgs);
        e.decide();
        assert!(e.nudged.contains(&2) && e.nudged.contains(&4), "{:?}", e.nudged);
        assert!(!e.stats.contains_key("miss_nudge"), "{:?}", e.stats);
        // a view rewritten anyway takes the earlier one out
        msgs.extend(step(3, "cat part3.txt", &listing(121, 180)));
        e.sync(&msgs);
        assert_eq!(e.nudge(e.tr.msgs.len(), true), Some(2));
        assert_eq!(e.nudged.iter().copied().collect::<Vec<_>>(), vec![6]);
    }

    #[test]
    fn a_note_that_puts_the_requests_for_notes_back_counts_the_miss() {
        let mut e = Engine::new(params("nudge=0;settle=50;eoq=0;check=100000000;every=100000").unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for (k, (a, b)) in [(1, 60), (61, 120)].into_iter().enumerate() {
            msgs.extend(step(k + 1, &format!("cat part{}.txt", k + 1), &listing(a, b)));
            e.sync(&msgs);
            e.decide();
        }
        assert!(e.nudged.contains(&2), "{:?}", e.nudged);
        let before = e.view();
        msgs.push(json!({"role": "assistant", "content": "Finding: slot 50 holds the longest name."}));
        msgs.push(json!({"role": "user", "content": "Thanks."}));
        e.sync(&msgs);
        e.decide();
        let after = e.view();
        // message 2 goes back to how it was sent before its request for notes: the cache misses from it
        assert_ne!(before[2], after[2]);
        let behind: i64 = after[2..].iter().map(crate::json::est).sum();
        assert!(e.stats.get("miss_note").is_some_and(|&m| m * 10 >= behind * 9), "{behind} {:?}", e.stats);
        assert!(e.stats["miss"] >= e.stats["miss_note"], "{:?}", e.stats);
    }

    #[test]
    fn what_leaves_for_the_window_counts_the_miss_from_the_anchor() {
        let mut e = Engine::new(params("nudge=0;eoq=0;close=0;live=0;check=100000000;every=100000").unwrap());
        // the request holds a long paste: the anchor is a large part of the view
        let ask = format!("Say what slot 50 holds in this listing:\n{}", listing(1000, 1150));
        let mut msgs = vec![json!({"role": "user", "content": ask})];
        for k in 0..6 {
            msgs.extend(step(k + 1, &format!("cat part{k}.txt"), &listing(60 * k + 1, 60 * k + 60)));
        }
        e.sync(&msgs);
        e.decide();
        let total = e.tokens();
        e.set_budget(total / 2);
        msgs.extend(step(7, "cat part7.txt", &listing(361, 380)));
        e.sync(&msgs);
        // `leave` writes the anchor again: the view as weighed before the window is fit misses whole
        let weighed: i64 = e.sizes_now().iter().sum();
        e.decide();
        assert!(e.stats.get("left_messages").is_some_and(|&n| n > 0), "{:?}", e.stats);
        assert_eq!(e.stats["miss_fit"], weighed, "{:?}", e.stats);
        assert!(e.stats["miss"] >= weighed, "{:?}", e.stats);
    }

    #[test]
    fn past_the_cap_only_copies_leave_until_twice_the_cap() {
        let spec =
            (0..200).map(|k| format!("field_{k} = {} {}", word(k), word(k + 1000))).collect::<Vec<_>>().join("\n");
        let log =
            (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "What does the spec say the log should hold?"})];
        msgs.extend(step(1, "cat spec.txt", &spec));
        msgs.extend(step(2, "cat run.log", &log));
        msgs.extend(step(3, "cat run.log", &log));
        for k in 4..11 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let results = |view: &[Value]| -> Vec<String> {
            view.iter()
                .flat_map(|m| blocks(m.get("content")).into_owned())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| text_of(b.get("content")).into_owned())
                .collect()
        };
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        // a target far below the view: the copy goes, and nothing else while the view is under twice the cap
        let given = format!("threshold=0;nudge=0;cap={};floor=0.2;cut=copies", total - 1);
        let mut e = Engine::new(params(&given).unwrap());
        e.sync(&msgs);
        let kept = results(&e.view());
        assert!(kept.contains(&spec), "{kept:?}");
        assert_eq!(kept.iter().filter(|r| **r == log).count(), 1, "{kept:?}");
        assert!(!e.stats.contains_key("left_messages") && !e.stats.contains_key("need_chunks"), "{:?}", e.stats);
        // past twice the cap, the cut that loses material runs as before
        let given = format!("threshold=0;nudge=0;cap={};floor=0.6;cut=copies", total / 4);
        let mut e = Engine::new(params(&given).unwrap());
        e.sync(&msgs);
        assert!(!results(&e.view()).contains(&spec));
    }
    #[test]
    fn past_the_cap_what_tells_the_most_stays_whatever_its_age() {
        let table = |base: usize| -> String {
            (0..200)
                .map(|k| format!("row {} holds {}", word(base + k), word(base + k + 500)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (spec, log, other) = (table(0), table(2000), table(4000));
        let asked = word(10);
        let results = |view: &[Value]| -> Vec<String> {
            view.iter()
                .flat_map(|m| blocks(m.get("content")).into_owned())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                .map(|b| text_of(b.get("content")).into_owned())
                .collect()
        };
        // the spec read first, the log read twice, a third table nobody asked about, then short steps
        let mut msgs = vec![json!({"role": "user", "content": format!("What does row {asked} of the spec hold?")})];
        msgs.extend(step(1, "cat spec.txt", &spec));
        msgs.extend(step(2, "cat run.log", &log));
        msgs.extend(step(3, "cat run.log", &log));
        msgs.extend(step(4, "cat other.txt", &other));
        for k in 5..12 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        // room for all but about one table and a half
        let cap = total - 1;
        let floor = (total - 3 * est_str(&log) / 2) as f64 / cap as f64;
        let mut e = Engine::new(params(&format!("threshold=0;nudge=0;cap={cap};floor={floor};cut=cover")).unwrap());
        e.sync(&msgs);
        let kept = results(&e.view());
        let all = kept.join("\n");
        let row = format!("row {asked} holds");
        assert!(all.contains(&row), "the oldest output holds the row the request asks about: {:?}", e.stats);
        // a second copy tells nothing: no line of the log is sent twice
        assert!(log.split('\n').all(|l| all.matches(l).count() <= 1), "{kept:?}");
        assert!(
            !e.stats.contains_key("left_messages") && e.stats.get("cover_chunks").is_some_and(|n| *n > 0),
            "{:?}",
            e.stats
        );
    }

    #[test]
    fn an_output_sent_as_a_pointer_to_an_earlier_one_is_not_cut_for_lines_it_no_longer_sends() {
        // reads of the same lines again go as pointers to the first: cutting them frees next to nothing
        let mut e =
            reads("dedup=3;nudge=0;eoq=0;protect=0;threshold=1;still=0", &[(1, 40), (1, 40), (1, 40), (41, 80)]);
        e.view();
        let (t, k) = (e.tr.msgs.len(), e.tr.steps.len());
        let sizes = e.sizes_now();
        e.evict(t, k, &sizes, true);
        let view = e.render();
        for m in [4, 6] {
            assert!(output(&view, m).contains("the same as lines 1-40 of the output of"), "{m}: {view:?}");
        }
        assert!(output(&view, 8).contains("left out here"), "{view:?}");
    }

    #[test]
    fn a_run_an_earlier_output_holds_points_to_it_while_that_output_is_whole() {
        let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60)]);
        let out = output(&e.view(), 4);
        let head = "[curator] 20 lines left out here: the same as lines 21-40 of the output of `sed -n '1,40p' listing.txt` above, from \"1108:";
        assert!(out.starts_with(head), "{out}");
        assert!(!out.contains("slot 30") && out.contains("slot 41\n") && out.ends_with("slot 60"), "{out}");
        assert!(reads("nudge=0", &[(1, 40), (21, 60)]).tokens() > e.tokens() + 100);
        // the earlier output is cut: the run is sent whole again
        let src = e.fz.by_msg(2)[0];
        e.dropped.entry(src).or_default().insert(0);
        assert_eq!(output(&e.render(), 4), listing(21, 60));
    }

    #[test]
    fn an_output_asked_for_past_its_pointer_goes_whole() {
        let reads3 = |spec: &str| {
            let view = reads(spec, &[(1, 40), (1, 40), (1, 40)]).view();
            (output(&view, 4), output(&view, 6))
        };
        let (second, third) = reads3("dedup=3;nudge=0;asked=2");
        // the first copy asked again points up; asked past that pointer, the lines come whole
        assert!(second.starts_with("[curator] 40 lines left out here: the same as lines 1-40"), "{second}");
        assert_eq!(third, listing(1, 40));
        let (second, third) = reads3("dedup=3;nudge=0;asked=1");
        assert_eq!((second.as_str(), third.as_str()), (listing(1, 40).as_str(), listing(1, 40).as_str()));
        let (second, third) = reads3("dedup=3;nudge=0");
        assert!(second.starts_with("[curator]") && third.starts_with("[curator]"), "{second}\n{third}");
    }

    #[test]
    fn the_output_just_arrived_goes_whole_and_points_up_from_the_next_request_on() {
        let mut e = reads("dedup=3;nudge=0;asked=3;fresh=1", &[(1, 40), (1, 40)]);
        // what the agent has just asked for comes as it is, with no pointer to send it looking
        assert_eq!(output(&e.view(), 4), listing(1, 40));
        let mut msgs = e.tr.msgs.clone();
        msgs.extend(step(9, "wc -l listing.txt", "40 listing.txt"));
        e.sync(&msgs);
        let view = e.view();
        assert!(output(&view, 4).starts_with("[curator] the same 40 lines as "), "{}", output(&view, 4));
        assert_eq!(output(&view, 6), "40 listing.txt");
    }

    #[test]
    fn an_output_the_same_call_gave_before_goes_as_one_line_that_says_nothing_changed() {
        let mut e = reads("dedup=3;nudge=0;asked=3", &[(1, 40), (1, 40), (21, 60)]);
        let view = e.view();
        let again = output(&view, 4);
        assert!(again.starts_with("[curator] the same "), "{again}");
        assert!(again.contains("`sed -n '1,40p' listing.txt` gave above: nothing changed"), "{again}");
        assert!(!again.contains("slot 30"), "{again}");
        // another call whose output is mostly lines above was asked for: it goes whole
        assert_eq!(output(&view, 6), listing(21, 60));
        // once the copy above loses a part, the lines come back whole
        let src = e.fz.by_msg(2)[0];
        e.dropped.entry(src).or_default().insert(0);
        assert_eq!(output(&e.render(), 4), listing(1, 40));
    }

    #[test]
    fn an_output_another_call_gave_before_goes_as_one_line_while_that_copy_is_whole() {
        let mut e = Engine::new(params("fresh=0;dedup=3;nudge=0;asked=3").unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 30 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "head -n 40 listing.txt", &listing(1, 40)));
        e.sync(&msgs);
        let again = output(&e.view(), 4);
        assert_eq!(again, "[curator] the same 40 lines as `sed -n '1,40p' listing.txt` gave above", "{again}");
        // once the copy above loses a part, the lines come back whole
        let src = e.fz.by_msg(2)[0];
        e.dropped.entry(src).or_default().insert(0);
        assert_eq!(output(&e.render(), 4), listing(1, 40));
    }

    #[test]
    fn lines_read_many_times_go_once_after_their_first_output_leaves() {
        let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60), (21, 60), (21, 60)]);
        let src = e.fz.by_msg(2)[0];
        e.dropped.entry(src).or_default().insert(0);
        let view = e.render();
        // the first copy carries the lines again, the later ones point to it
        assert_eq!(output(&view, 4), listing(21, 60));
        for m in [6, 8] {
            let out = output(&view, m);
            assert!(out.starts_with("[curator] 20 lines left out here: the same as lines 1-20 of the output of `sed -n '21,60p' listing.txt` above"), "{out}");
            assert!(!out.contains("slot 30"), "{out}");
        }
    }

    #[test]
    fn a_pointer_that_halves_its_lines_goes_in_their_place() {
        let mut e = reads("dedup=3;nudge=0", &[(1, 40), (11, 19)]);
        let out = output(&e.view(), 4);
        let (run, cost) = (est_str(&listing(11, 19)), est_str(&out));
        assert!(2 * cost <= run && run < 3 * cost, "{run} {cost}");
        assert!(out.starts_with("[curator] 9 lines left out here: the same as lines 11-19"), "{out}");
    }

    #[test]
    fn a_read_made_after_its_first_output_lost_a_part_points_to_the_copy_that_sends_it() {
        let mut e = Engine::new(params("dedup=3;nudge=0;fresh=0").unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "sed -n '21,60p' listing.txt", &listing(21, 60)));
        e.sync(&msgs);
        let src = e.fz.by_msg(2)[0];
        e.dropped.entry(src).or_default().insert(0);
        msgs.extend(step(3, "sed -n '21,60p' listing.txt", &listing(21, 60)));
        e.sync(&msgs);
        let view = e.render();
        assert_eq!(output(&view, 4), listing(21, 60));
        let out = output(&view, 6);
        assert!(out.contains("the same as lines 1-20 of the output of `sed -n '21,60p' listing.txt` above"), "{out}");
        assert!(!out.contains("slot 30"), "{out}");
    }

    #[test]
    fn a_part_read_again_later_leaves_the_earlier_copy_and_the_latest_stays_whole() {
        let spec = "nudge=0;eoq=0;protect=0;threshold=0;margin=0;sup=1";
        let ranges = [(1, 40), (1, 40), (61, 100), (101, 140), (141, 180)];
        let mut e = reads(spec, &ranges);
        let view = e.view();
        assert!(output(&view, 2).contains("a later output below sends every line of it"), "{:?}", e.stats);
        assert_eq!(output(&view, 4), listing(1, 40));
        // without it, both copies stay
        let mut e = reads("nudge=0;eoq=0;protect=0;threshold=0;margin=0", &ranges);
        assert_eq!(output(&e.view(), 2), listing(1, 40));
    }

    #[test]
    fn what_the_agent_has_not_written_about_stays_through_a_new_request() {
        let view_of = |spec: &str| {
            let mut e = reads(spec, &[(1, 40), (41, 80), (81, 120)]);
            let mut msgs = e.tr.msgs.clone();
            msgs.push(json!({"role": "user", "content": "And what does slot 200 hold?"}));
            for k in 4..9 {
                msgs.extend(step(
                    k,
                    &format!("sed -n '{},{}p' other.txt", k * 50, k * 50 + 39),
                    &listing(k * 50, k * 50 + 39),
                ));
            }
            e.sync(&msgs);
            e.view()
        };
        let spec = "nudge=0;eoq=0;protect=0;threshold=1;margin=0;every=1;check=0";
        // no text and no note since the first reads: the agent is still working from them
        assert_eq!(output(&view_of(&format!("{spec};still=100000")), 2), listing(1, 40));
        // a guess that they are not needed takes them otherwise
        assert_ne!(output(&view_of(&format!("{spec};still=0")), 2), listing(1, 40));
    }

    #[test]
    fn live_drop_keeps_part_carrying_a_copy() {
        let log =
            (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
        let mut msgs = vec![json!({"role": "user", "content": "What does the log hold?"})];
        msgs.extend(step(1, "cat run.log", &log));
        msgs.extend(step(2, "cat run.log", &log));
        for k in 3..8 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        let seen = |view: &[Value]| -> (usize, bool) {
            let text: String = view.iter().map(|m| crate::msg::flat(m.get("content")) + "\n").collect();
            let lines: FxHashSet<&str> = text.lines().filter(|l| l.starts_with("event ")).collect();
            (lines.len(), text.contains("a later output below sends"))
        };
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        let cap = total - 1;
        let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
        let given = format!(
            "threshold=1;still=0;protect=2;nudge=0;sup=1;cut=copies;cap={cap};floor={floor};live=1;close=2;margin=0"
        );
        let mut e = Engine::new(params(&given).unwrap());
        e.sync(&msgs);
        let (lines, stub) = seen(&e.view());
        assert!(stub && lines == 200 && !e.carrying.is_empty(), "{:?}", e.stats);
        // more steps the agent never writes about: the idle outputs of the open request may leave, never the
        // one that sends the lines of the copy left out
        for k in 9..14 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
            e.sync(&msgs);
            let (lines, stub) = seen(&e.view());
            assert!(!stub || lines == 200, "request {k}: {lines} lines, {:?}", e.stats);
        }
    }
    #[test]
    fn close_line_survives_start_inside_request() {
        // a first request answered, then one whose request message leaves the view while its calls stay
        let mut msgs =
            vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Client X."}]}));
        msgs.push(json!({"role": "user", "content": "How long has the host been up?"}));
        msgs.extend(step(1, "date", "Fri Oct 31 10:00:00 UTC 2026"));
        msgs.extend(step(2, "uptime", "10:00 up 3 days"));
        msgs.extend(step(3, "who", "root tty1"));
        msgs.extend(step(4, "wc -l report.txt", "530 report.txt"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Up 3 days."}]}));
        msgs.push(json!({"role": "user", "content": "Thanks."}));
        let mut e = Engine::new(params("nudge=0;eoq=0;close=1;margin=1000").unwrap());
        e.sync(&msgs);
        e.view();
        assert_eq!((e.steps.len(), e.steps.iter().filter(|s| s.out).count()), (4, 0));
        // the view starts at the second call: the request message is no longer sent
        (e.start, e.vis) = (5, None);
        (e.steps[1].out, e.steps[2].out) = (true, true);
        e.shut_lines();
        let view = sent(&e.view());
        assert!(view.contains("2 tool calls made for this request"), "{:?} {view}", e.shut);
        assert!(view.contains("bash uptime | bash who."), "{view}");
        assert!(e.shut.keys().all(|&mi| mi >= e.start), "{:?}", e.shut);
        // the run reaches the last call: its line is still sent, after the reply, and says no reply follows
        e.steps[3].out = true;
        e.shut_lines();
        let view = sent(&e.view());
        assert!(view.contains("3 tool calls made for this request") && !view.contains("follows"), "{view}");
        assert!(e.shut.keys().all(|&mi| mi >= e.start), "{:?}", e.shut);
    }

    #[test]
    fn cards_negative_limit_counts_hidden() {
        let mut msgs: Vec<Value> = Vec::new();
        for k in 0..6 {
            if k > 0 {
                msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": format!("It is {}.", word(k + 50))}]}));
            }
            msgs.push(json!({"role": "user", "content": format!("What does {} stand for?", word(k))}));
        }
        let mut e = Engine::new(params("nudge=0;eoq=0;link=1;apart=0").unwrap());
        // the settings take no negative limit, a `Params` built in code can: all but the two oldest shown
        e.p.cards = -2;
        e.sync(&msgs);
        e.view();
        assert_eq!(e.goal.others().len(), 5);
        let text = e.cards_text();
        // five other topics, all but two shown: two are not
        assert!(text.ends_with("• … and 2 older topics."), "{text}");
        assert_eq!(text.lines().count(), 1 + 3 + 1, "{text}");
    }

    /// Whether every tool result of `view` has its call in the view, and every call its result.
    fn paired(view: &[Value]) -> bool {
        let ids = |kind: &str, key: &str| -> Vec<String> {
            let mut v: Vec<String> = view
                .iter()
                .flat_map(|m| blocks(m.get("content")).into_owned())
                .filter(|b| b.get("type").and_then(Value::as_str) == Some(kind))
                .filter_map(|b| b.get(key).and_then(Value::as_str).map(str::to_string))
                .collect();
            v.sort();
            v
        };
        ids("tool_use", "id") == ids("tool_result", "tool_use_id")
    }

    #[test]
    fn with_task_a_request_on_results_keeps_calls_and_results_together() {
        // the person's next request rides on the results of a step, on another topic
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for k in 0..3 {
            msgs.extend(step(
                k + 1,
                &format!("sed -n '{},{}p' listing.txt", 10 * k + 1, 10 * k + 10),
                &listing(10 * k + 1, 10 * k + 10),
            ));
        }
        let last = msgs.len() - 1;
        let mut bs = blocks(msgs[last].get("content")).into_owned();
        bs.push(json!({"type": "text", "text": "Stop that. How many lines has notes.md?"}));
        msgs[last]["content"] = Value::Array(bs);
        msgs.extend(step(4, "wc -l notes.md", "12 notes.md"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": "And what does slot 7 of listing.txt hold?"}));
        for spec in
            ["nudge=0;eoq=0;task=1;link=1;apart=0", "nudge=0;eoq=0;task=1;link=1;apart=0;close=2;margin=0;ahead=1"]
        {
            let mut e = Engine::new(params(spec).unwrap());
            for t in [last + 1, last + 3, msgs.len()] {
                e.sync(&msgs[..t]);
                let view = e.view();
                assert!(paired(&view), "{spec}, {t} messages: {}", sent(&view));
                let text = sent(&view);
                // the request left for a newer one had no reply
                assert!(!text.contains("The reply that closed the request follows"), "{spec}: {text}");
            }
        }
    }
    #[test]
    fn a_pointer_points_to_lines_sent_whole_and_short_runs_stay() {
        let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60), (22, 38), (42, 58), (58, 70)]);
        let view = e.view();
        // lines the second read sends as a pointer are found in the first
        assert!(output(&view, 6).contains("lines 22-38 of the output of `sed -n '1,40p' listing.txt`"), "{view:?}");
        assert!(output(&view, 8).contains("lines 22-38 of the output of `sed -n '21,60p' listing.txt`"), "{view:?}");
        // a line in common is fewer tokens than twice its pointer
        assert_eq!(output(&view, 10), listing(58, 70));
        assert_eq!(output(&reads("nudge=0", &[(1, 40), (21, 60)]).view(), 4), listing(21, 60), "off by default");
    }

    /// A report with a line per invoice, every line its own words.
    fn report() -> String {
        (0..30)
            .map(|k| format!("invoice {} total {} client {}", word(k), 1000 + 37 * k, word(k + 500)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A first request answered from a file read, a second answered from a short command, and `third` if
    /// some: what the person asks next.
    fn sessions(reply: &str, third: Option<&str>) -> Vec<Value> {
        let mut msgs =
            vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
        msgs.extend(step(1, "cat report.txt", &report()));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": reply}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
        if let Some(q) = third {
            msgs.extend(step(2, "wc -l notes.md", "12 notes.md"));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
            msgs.push(json!({"role": "user", "content": q}));
        }
        msgs
    }

    fn sent(view: &[Value]) -> String {
        serde_json::to_string(view).unwrap()
    }

    #[test]
    fn the_size_cap_leaves_alone_what_the_agent_has_not_written_about_yet() {
        // reads one after another with no word between them: the agent is still reading, and what it read
        // stays; once it writes about it, the cap counts it again
        let mut msgs = vec![json!({"role": "user", "content": "Which slot holds the longest name in the listing?"})];
        for n in 0..8 {
            msgs.extend(step(
                n,
                &format!("sed -n {},{}p listing.txt", 40 * n + 1, 40 * n + 40),
                &listing(40 * n + 1, 40 * n + 40),
            ));
        }
        let all = |view: &str| (0..8).all(|n| view.contains(&word(40 * n + 120)[..]));
        for (hold, kept) in [("hold=0", false), ("hold=1", true)] {
            let mut e = Engine::new(params(&format!("nudge=0;protect=0;eoq=500;{hold}")).unwrap());
            for n in (1..=msgs.len()).step_by(2) {
                e.sync(&msgs[..n]);
                e.view();
            }
            assert_eq!(all(&sent(&e.view())), kept, "{hold}");
        }
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Slot 7 so far; reading on."}]}));
        msgs.push(json!({"role": "user", "content": "go on"}));
        let mut e = Engine::new(params("nudge=0;protect=0;eoq=500;hold=1").unwrap());
        e.sync(&msgs);
        assert!(!all(&sent(&e.view())));
    }

    #[test]
    fn a_session_where_the_agent_never_writes_still_has_a_bounded_view() {
        // what the cap leaves out of its count is only the latest outputs that fit `still`: past that the older
        // ones count, or an agent that only reads would grow the view past any cap
        let mut msgs = vec![json!({"role": "user", "content": "Which slot holds the longest name in the listing?"})];
        for n in 0..40 {
            msgs.extend(step(
                n,
                &format!("sed -n {},{}p listing.txt", 40 * n + 1, 40 * n + 40),
                &listing(40 * n + 1, 40 * n + 40),
            ));
        }
        let most = |spec: &str| {
            let mut e = Engine::new(params(&format!("nudge=0;protect=0;eoq=500;hold=1;{spec}")).unwrap());
            let mut most = 0;
            for n in (1..=msgs.len()).step_by(2) {
                e.sync(&msgs[..n]);
                e.view();
                most = most.max(e.tokens());
            }
            (most, e.raw_tokens)
        };
        let (unbounded, all) = most("still=0");
        let (bounded, _) = most("still=2000");
        assert!(unbounded * 10 > all * 9, "with no budget for it, all that is held stays: {unbounded} of {all}");
        assert!(bounded * 3 < all, "past the budget, the cap takes the older outputs: {bounded} of {all}");
    }

    /// A view with what a model cannot tell apart left out: call ids and the numbers in curator lines.
    fn shape(view: &[Value]) -> String {
        let mut s = sent(view);
        for k in 0..40 {
            s = s.replace(&format!("call_{k}\""), "call\"");
        }
        s.lines()
            .map(|l| {
                if l.contains(MARK) {
                    l.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    #[test]
    fn the_default_never_sends_a_view_it_sent_before_when_the_agent_repeats_a_step() {
        // at a fixed temperature a model sent the same view takes the same step again: the history must grow
        let mut msgs = vec![json!({"role": "user", "content": "Which blocks jump to the handler?"})];
        msgs.extend(step(0, "sed -n 1,40p listing.txt", &listing(1, 40)));
        let mut e = Engine::new(crate::params("").unwrap());
        let mut seen = FxHashSet::default();
        for k in 1..=12 {
            msgs.extend(step(k, "sed -n 1,40p listing.txt", &listing(1, 40)));
            e.sync(&msgs);
            assert!(seen.insert(shape(&e.view())), "step {k} sent a view sent before");
        }
    }

    #[test]
    fn a_step_made_again_with_the_same_output_keeps_its_first_and_latest_run_and_a_count() {
        // the agent runs the same search again and again and gets the same nothing back; then something new
        let mut msgs = vec![json!({"role": "user", "content": "Which blocks jump to the handler?"})];
        msgs.extend(step(0, "sed -n 1,40p listing.txt", &listing(1, 40)));
        for k in 1..=6 {
            msgs.extend(step(k, "grep -n 'jmp handler' listing.txt", "(no output)"));
        }
        let runs = |view: &[Value]| sent(view).matches("jmp handler").count();
        let mut e = Engine::new(params("nudge=0;loops=0").unwrap());
        e.sync(&msgs);
        assert_eq!(runs(&e.view()), 6);
        let mut e = Engine::new(params("nudge=0;loops=1").unwrap());
        for n in (3..=msgs.len()).step_by(2) {
            e.sync(&msgs[..n]);
            e.view();
        }
        let view = e.view();
        assert_eq!(runs(&view), 2, "{view:?}");
        assert!(sent(&view[view.len() - 1..]).contains("run 6 times in a row"), "{view:?}");
        assert_eq!(sent(&view).matches("times in a row").count(), 1);
        assert!(sent(&view).contains(&word(120)[..]));
        // a step that differs ends the run: what left stays out, the new step comes whole
        msgs.extend(step(7, "grep -n 'jmp' listing.txt", "12: jmp short loc_1F20"));
        msgs.extend(step(8, "grep -n 'jmp handler' listing.txt", "(no output)"));
        e.sync(&msgs);
        let view = e.view();
        assert_eq!(runs(&view), 3, "{view:?}");
        assert!(!sent(&view[view.len() - 1..]).contains("times in a row"));
        assert_eq!(e.stats.get("looped_steps"), Some(&4));
    }

    #[test]
    fn an_answered_request_lets_its_steps_go_and_a_request_about_them_brings_them_back() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&format!("The largest is client {}.", word(529)), None));
        let view = sent(&e.view());
        let before = e.view();
        assert!(!view.contains(&word(0)[..]), "{view}");
        assert!(view.contains("are left out of this context: bash cat report.txt"), "{view}");
        // the person goes back to the invoices: the read comes back, at the end of the view
        let q = "And the smallest invoice total in the report?";
        e.sync(&sessions(&format!("The largest is client {}.", word(529)), Some(q)));
        let view = e.view();
        let last = sent(&view[view.len() - 1..]);
        assert!(last.contains("Brought back") && last.contains(&word(0)), "{last}");
        assert_eq!(e.stats.get("recalled_steps"), Some(&1));
        // the line that says the read left stays as it was, so the cache keeps all before the new messages
        assert_eq!(sent(&view[..1]), sent(&before[..1]));
    }

    #[test]
    fn a_request_closed_after_a_read_came_back_to_it_keeps_what_came_back_first() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let reply = format!("The largest is client {}.", word(529));
        let report = report();
        let low = report.lines().next().unwrap().rsplit(' ').next().unwrap();
        let mut msgs = sessions(&reply, Some("And the smallest invoice total in the report?"));
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&reply, None));
        e.view();
        e.sync(&msgs);
        let before = e.view();
        let mi = before.len() - 1;
        assert!(sent(&before[mi..]).contains("Brought back"));
        // that request runs a call of its own and is answered, and the next one is on the report again: the
        // read stays, the call leaves, and the line that says so comes after what came back
        msgs.extend(step(3, "date", "Fri Oct 31 10:00:00 UTC 2026"));
        let answer = format!("The smallest is client {low}.");
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": answer}]}));
        msgs.push(json!({"role": "user", "content": "And which client has the invoice total of 1037 in report.txt?"}));
        e.sync(&msgs);
        let view = e.view();
        let (was, now) = (blocks(before[mi].get("content")).into_owned(), blocks(view[mi].get("content")).into_owned());
        assert!(now.len() > was.len() && now[..was.len()] == was[..], "{was:?}\n{now:?}");
    }

    /// The agent's `recall` of message `mi` (lines `from`-`to` of it, when given) and the answer the host
    /// gives it, `text`.
    fn recall_step(k: usize, mi: usize, part: Option<(usize, usize)>, text: &str) -> [Value; 2] {
        let id = format!("recall_{k}");
        let mut input = json!({"message": format!("m{:05}", mi + 1)});
        let lines = match part {
            Some((from, to)) => {
                input["from"] = json!(from);
                input["to"] = json!(to);
                format!(", lines {from}-{to} of {}", text.lines().count())
            }
            None => String::new(),
        };
        let body: String = match part {
            Some((from, to)) => text.lines().skip(from - 1).take(to + 1 - from).collect::<Vec<_>>().join("\n"),
            None => text.to_string(),
        };
        [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "recall", "input": input}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id,
                "content": format!("m{:05} (the output of bash){lines}, as it was:\n{body}", mi + 1)}]}),
        ]
    }

    /// Feeds `msgs` to `e` as a host does: a view before each call of the model (after each message of the
    /// person, a request or the results of a step).
    fn feed(e: &mut Engine, msgs: &[Value]) {
        for t in 1..=msgs.len() {
            if msgs[t - 1]["role"] == "user" && t > e.tr.msgs.len() {
                e.sync(&msgs[..t]);
                e.view();
            }
        }
    }

    #[test]
    fn the_agent_recalling_an_output_the_view_left_out_counts_its_step_as_brought_back() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let reply = format!("The largest is client {}.", word(529));
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = sessions(&reply, None);
        feed(&mut e, &msgs);
        assert!(sent(&e.view()).contains("are left out of this context: bash cat report.txt"));
        assert_eq!((e.steps[0].out, e.steps[0].back), (true, 0));
        // the agent asks for the read back by its name (message 3): the step was needed after it left
        msgs.extend(recall_step(1, 2, None, &report()));
        feed(&mut e, &msgs);
        assert_eq!(e.steps[0].back, 1);
        assert_eq!(e.stats.get("agent_recalled_steps"), Some(&1));
        // a recall of a message the view sends (the reply, the open request) is the agent's own doing
        msgs.extend(recall_step(2, 3, None, &reply));
        feed(&mut e, &msgs);
        msgs.extend(recall_step(3, 4, None, "How many lines has notes.md?"));
        feed(&mut e, &msgs);
        assert_eq!((e.steps[0].back, e.stats.get("agent_recalled_steps")), (1, Some(&1)));
        // the next request is on the report: the curator brings the read back; a recall of it then is the
        // agent's own doing too, since its text is in the view
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "3 lines came back."}]}));
        msgs.push(json!({"role": "user", "content": "And the smallest invoice total in the report?"}));
        feed(&mut e, &msgs);
        assert_eq!((e.steps[0].out, e.steps[0].at.is_some(), e.steps[0].back), (false, true, 2));
        msgs.extend(recall_step(4, 2, None, &report()));
        feed(&mut e, &msgs);
        assert_eq!((e.steps[0].back, e.stats.get("agent_recalled_steps")), (2, Some(&1)));
    }

    #[test]
    fn a_read_made_again_counts_as_brought_back_only_when_its_earlier_output_was_left_out() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let reply = format!("The largest is client {}.", word(529));
        // the read of the first request left: the agent runs it again on the second
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = sessions(&reply, None);
        feed(&mut e, &msgs);
        msgs.extend(step(2, "cat  report.txt", &report()));
        feed(&mut e, &msgs);
        assert_eq!((e.steps[0].back, e.stats.get("agent_recalled_steps")), (1, Some(&1)));
        // the read stays in the view (no closing): running it again is the agent's own doing
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0").unwrap());
        let mut msgs = sessions(&reply, None);
        feed(&mut e, &msgs);
        msgs.extend(step(2, "cat report.txt", &report()));
        feed(&mut e, &msgs);
        assert_eq!(e.stats.get("agent_recalled_steps"), None);
        // the file was edited since the read left: the agent reads what it holds now, not what left
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = sessions(&reply, None);
        feed(&mut e, &msgs);
        msgs.extend(step(2, "sed -i 's/invoice/bill/' report.txt", ""));
        feed(&mut e, &msgs);
        msgs.extend(step(3, "cat report.txt", &report().replace("invoice", "bill")));
        feed(&mut e, &msgs);
        assert_eq!(e.stats.get("agent_recalled_steps"), None);
        // a command that runs a process, not one that prints files, is no read of what left
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Do the tests pass?"})];
        msgs.extend(step(1, "cargo test", &report()));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "They pass."}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
        feed(&mut e, &msgs);
        assert!(e.steps[0].out);
        msgs.extend(step(2, "cargo test", &report()));
        feed(&mut e, &msgs);
        assert_eq!(e.stats.get("agent_recalled_steps"), None);
    }

    /// A read of lines `a..=b` of `listing.txt` by a read tool, with its output.
    fn read_step(k: usize, (a, b): (usize, usize)) -> [Value; 2] {
        let id = format!("call_{k}");
        [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": "read",
                "input": {"path": "listing.txt", "offset": a, "limit": b + 1 - a}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": listing(a, b)}]}),
        ]
    }

    #[test]
    fn a_read_over_lines_an_output_that_left_gave_counts_and_the_step_made_of_it_later_starts_with_the_count() {
        // the first two reads leave unused (`live`) while the request is open, the third stays
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        for (k, r) in [(1, 40), (41, 80), (81, 120)].into_iter().enumerate() {
            msgs.extend(read_step(k + 1, r));
        }
        feed(&mut e, &msgs);
        let view = e.view();
        assert!(output(&view, 2).contains("left out here") && output(&view, 4).contains("left out here"));
        assert_eq!(output(&view, 6), listing(81, 120));
        // lines the view still sends, and lines nothing read yet, are no asking back
        msgs.extend(read_step(4, (100, 110)));
        feed(&mut e, &msgs);
        msgs.extend(read_step(5, (121, 160)));
        feed(&mut e, &msgs);
        assert_eq!(e.stats.get("agent_recalled_steps"), None);
        // lines of the two outputs that left: the latest of them is asked back
        msgs.extend(read_step(6, (20, 50)));
        feed(&mut e, &msgs);
        assert_eq!((e.stats.get("agent_recalled_steps"), e.brought.get(&4)), (Some(&1), Some(&1)));
        // the request is answered: the step made of the second read starts as brought back once
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Slot 50 holds nothing."}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
        feed(&mut e, &msgs);
        assert_eq!(e.steps.iter().map(|s| s.back).collect::<Vec<_>>(), vec![0, 1, 0, 0, 0, 0]);
        assert!(e.brought.is_empty());
    }

    #[test]
    fn sticky_a_step_brought_back_leaves_again_later_and_one_the_agent_asked_back_later_still() {
        let base = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let reply = format!("The largest is client {}.", word(529));
        // request 1 read the report and request 2 is on another file; on it the agent may ask three lines of
        // the read back; request 3 is on the report again, so the curator brings the read back; request 4
        // is on another file again, and the read goes idle
        let session = |recalled: bool| {
            let mut msgs = sessions(&reply, None);
            if recalled {
                msgs.extend(recall_step(1, 2, Some((1, 3)), &report()));
            }
            msgs.extend(step(3, "wc -l notes.md", "12 notes.md"));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
            msgs.push(json!({"role": "user", "content": "And the smallest invoice total in the report?"}));
            msgs
        };
        let low = report().lines().next().unwrap().rsplit(' ').next().unwrap().to_string();
        // the steps the agent takes on request 4 before the read leaves again
        let leaves_after = |spec: &str, recalled: bool| -> usize {
            let mut e = Engine::new(params(spec).unwrap());
            let mut msgs = session(recalled);
            feed(&mut e, &msgs);
            assert_eq!((e.steps[0].out, e.steps[0].at.is_some()), (false, true), "the read came back");
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": format!("Client {low}.")}]}));
            msgs.push(json!({"role": "user", "content": "How many words has notes.md?"}));
            feed(&mut e, &msgs);
            let mut k = 0;
            while e.steps[0].at.is_some() && k < 20 {
                k += 1;
                msgs.extend(step(10 + k, "date", "Fri Oct 31 10:00:00 UTC 2026"));
                feed(&mut e, &msgs);
            }
            assert!(e.steps[0].out, "{spec}: the read never left");
            k
        };
        // without `sticky` an idle step leaves as soon as the rent to come pays; with it, once the rent paid
        // since it went idle (a tenth of its size per step) pays for bringing it back once more: half its size
        // for a step brought back once, two thirds for one the agent had asked back as well. The step of the
        // agent's recall, idle on request 4 with it, leaves at once and does not take it along.
        assert_eq!((leaves_after(base, false), leaves_after(base, true)), (0, 0));
        let sticky = format!("{base};sticky=1");
        assert_eq!((leaves_after(&sticky, false), leaves_after(&sticky, true)), (6, 7));
    }

    #[test]
    fn a_call_leaves_only_when_it_pays_for_the_view_read_again_from_the_request_it_was_made_for() {
        // the cut is priced from the request the call was made for: all read after it, the large read the
        // next request still works from too, is read again without the cache, not only what follows the call
        let mut msgs =
            vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
        msgs.extend(step(1, "cat report.txt", &report()));
        msgs.extend(step(2, "date", "Fri Oct 31 10:00:00 UTC 2026"));
        let reply = format!("The largest is client {}.", word(529));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": reply}]}));
        msgs.push(json!({"role": "user", "content": "And the smallest invoice total in report.txt?"}));
        let spec = |margin: &str| format!("nudge=0;eoq=0;link=0;close=2;named=16000;ahead=1;margin={margin}");
        let mut e = Engine::new(params(&spec("0.05")).unwrap());
        e.sync(&msgs);
        let view = sent(&e.view());
        assert!(view.contains(&word(0)[..]) && !view.contains("left out of this context"), "{view}");
        // when the cut costs nothing, the call leaves and the read the request names stays
        let mut e = Engine::new(params(&spec("0")).unwrap());
        e.sync(&msgs);
        let view = sent(&e.view());
        assert!(view.contains(&word(0)[..]) && view.contains("left out of this context: bash date"), "{view}");
    }

    #[test]
    fn the_line_for_calls_that_left_goes_just_before_them_and_all_ahead_stays_as_the_cache_holds_it() {
        // calls that leave from the middle of a long request are named on the output just before them, not
        // on the request: the view only changes from there on. Only the line of the calls up to the reply
        // says the reply follows.
        let mut msgs =
            vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
        msgs.extend(step(1, "cat report.txt", &report()));
        msgs.extend(step(2, "date", "Fri Oct 31 10:00:00 UTC 2026"));
        msgs.extend(step(3, "uptime", "10:00 up 3 days"));
        msgs.extend(step(4, "wc -l report.txt", "530 report.txt"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "The largest is client X."}]}));
        msgs.push(json!({"role": "user", "content": "Thanks."}));
        let mut e = Engine::new(params("nudge=0;eoq=0;close=1;margin=1000").unwrap());
        e.sync(&msgs);
        e.view();
        assert_eq!((e.steps.len(), e.steps.iter().filter(|s| s.out).count()), (4, 0));
        let line = |e: &Engine, mi: usize| e.shut.get(&mi).map(|l| l.join(" ")).unwrap_or_default();
        (e.steps[1].out, e.steps[2].out) = (true, true);
        assert_eq!(e.shut_lines(), Some(2));
        assert!(!e.shut.contains_key(&0) && line(&e, 2).contains("2 tool calls"), "{:?}", e.shut);
        assert!(line(&e, 2).contains("bash date | bash uptime.") && !line(&e, 2).contains("follows"));
        // the run grows to the last call: its line, the same place, now says the reply follows
        e.steps[3].out = true;
        assert_eq!(e.shut_lines(), Some(2));
        assert!(
            line(&e, 2).contains("3 tool calls") && line(&e, 2).contains("The reply that closed the request follows.")
        );
        // the first call leaves too: the run starts after the request, and the request takes the line
        e.steps[0].out = true;
        assert_eq!(e.shut_lines(), Some(0));
        assert!(e.shut.keys().eq([&0]) && line(&e, 0).contains("4 tool calls"), "{:?}", e.shut);
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]) && view.contains("4 tool calls made for this request"), "{view}");
    }

    #[test]
    fn by_topic_the_read_stays_while_the_requests_stay_on_it_and_leaves_when_they_move_on() {
        let spec = "nudge=0;eoq=0;close=2;margin=0;ahead=1;topic=1";
        let reply = format!("The largest is client {}.", word(529));
        let mut msgs = sessions(&reply, None);
        msgs.pop();
        msgs.push(json!({"role": "user", "content": "And the smallest invoice total?"}));
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&msgs);
        let view = sent(&e.view());
        assert!(view.contains(&word(0)[..]) && !view.contains("left out of this context"), "{view}");
        // without it the read stays too: the request names words ("invoice", "total") only that read holds
        let mut e = Engine::new(params("nudge=0;eoq=0;close=2;margin=0;ahead=1").unwrap());
        e.sync(&msgs);
        assert!(!sent(&e.view()).contains("left out of this context: bash cat report.txt"));
        // the next request is on something else: the read leaves with its topic
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&reply, None));
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]), "{:?} {view}", e.stats);
    }

    #[test]
    fn noted_the_read_leaves_once_a_note_names_its_file_and_not_before() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;noted=1";
        let largest = format!("The largest is client {}.", word(529));
        // answered by an agent that writes no notes: it is not judged by them, and the read leaves as it
        // does without `noted`
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&largest, None));
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]) && view.contains("left out of this context"), "{view}");
        // a note on something else names no file read: the read stays
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&format!("{largest}\nFinding: a client is a person who buys."), None));
        assert!(sent(&e.view()).contains(&word(0)[..]));
        // the note names the file: its facts are kept, the read leaves
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions(&format!("{largest}\nFinding: the report lists one invoice total per client."), None));
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]), "{view}");
        assert!(view.contains("are left out of this context: bash cat report.txt"), "{view}");
    }

    #[test]
    fn noted_a_read_leaves_once_a_note_names_its_file_and_a_later_request_on_it_needed_no_read() {
        // a request on the file, a read, a note that names it, the reply; a second request on the file; a
        // third one, still open, on the file again
        let ask = |q: &str| json!({"role": "user", "content": q});
        let say = |t: &str| json!({"role": "assistant", "content": [{"type": "text", "text": t}]});
        let session = |again: bool| {
            let mut msgs = vec![ask("Read listing.txt and say what slot 50 holds.")];
            msgs.extend(step(1, "cat listing.txt", &listing(1, 120)));
            msgs.push(json!({"role": "assistant", "content": [{"type": "tool_use", "id": "call_2", "name": NOTE_TOOL,
                "input": {"text": format!("Finding: the listing holds {} at slot 50, {} at 51.", word(50), word(51))}}]}));
            msgs.push(json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_2", "content": "noted"}]}));
            msgs.push(say(&format!("Slot 50 holds {}.", word(50))));
            msgs.push(ask("And slot 51 of listing.txt?"));
            if again {
                msgs.extend(step(3, "sed -n 51p listing.txt", &listing(51, 51)));
            }
            msgs.push(say(&format!("Slot 51 holds {}.", word(51))));
            msgs.push(ask("And slot 52 of listing.txt?"));
            msgs.extend(step(4, "date", "Sun Nov  1 10:00:00 UTC 2026"));
            msgs
        };
        let base = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1;named=16000;fresh=0";
        let read_left = |spec: &str, again: bool| {
            let mut e = Engine::new(params(&format!("{base};{spec}")).unwrap());
            e.sync(&session(again));
            let view = sent(&e.view());
            assert!(view.contains(&word(50)[..]), "the note stays: {view}");
            !view.contains(&word(120)[..])
        };
        // the second request was answered from the note: the read leaves, though the request names its file
        assert!(read_left("noted=1", false));
        assert!(!read_left("noted=0", false));
        // the agent went back to the file: the note was not enough, and the read stays
        assert!(!read_left("noted=1", true));
    }

    #[test]
    fn a_file_is_named_by_what_sets_it_apart() {
        assert_eq!(
            files_in("cd material; head -60 assembly-A.txt; sed -n 1,9p ../src/auth_db.rs"),
            vec![vec!["assembly".to_string(), "A".to_string()], vec!["auth".to_string(), "db".to_string()],]
        );
        assert!(files_in("wc -l material/* | grep -n \"a\\|b\" x -n 60,921p").is_empty());
        assert!(files_in("cd material; sed -n '/loc_35DE:/,/loc_36C9:/p' 1.5 src/").is_empty());
        assert_eq!(
            name_parts("Function A (assembly-A) and a word"),
            ["function", "A", "assembly", "A", "and", "a", "word"]
        );
    }

    #[test]
    fn rules_come_from_what_the_person_wrote_and_a_withdrawal_follows_the_rule_it_changes() {
        let rules = |e: &Engine| e.rules.list.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        let mut e = Engine::new(params("").unwrap());
        let first = vec![
            json!({"role": "user", "content": [
                {"type": "text", "text": "<system-reminder>\nNever edit the lockfile.\nYou must reply in French.\n</system-reminder>"},
                {"type": "text", "text": "Never use unwrap.\nERROR: not allowed to write /etc/x"}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "Understood."}]}),
            json!({"role": "user", "content": "you can use unwrap now"}),
        ];
        e.sync(&first);
        assert_eq!(rules(&e), ["Never use unwrap.", "you can use unwrap now"]);
        // a compaction: the later word stays after the rule it changes
        e.sync(&[json!({"role": "user", "content": "Summary: the parser is half done."})]);
        assert_eq!(rules(&e), ["Never use unwrap.", "you can use unwrap now"]);
        let block = e.rules.text(|_| true);
        assert!(block.find("Never use unwrap").unwrap() < block.find("you can use unwrap now").unwrap(), "{block}");
    }

    #[test]
    fn live_an_output_no_later_call_or_text_drew_on_leaves_in_its_place() {
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
        // two more reads of other lines follow the first, and neither names anything it printed
        let e = &mut reads(spec, &[(1, 40), (41, 80), (81, 120)]);
        let view = e.view();
        assert!(output(&view, 2).contains("left out here"), "{view:?}");
        assert_eq!(output(&view, 6), listing(81, 120));
        // the agent's next call names an address only the first output printed: it is in use and stays
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
        msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
        e.sync(&msgs);
        assert_eq!(output(&e.view(), 2), listing(1, 40));
    }

    #[test]
    fn live_prices_an_output_at_what_it_sends_now() {
        let mut e = reads("nudge=0;eoq=0;threshold=0", &[(1, 200)]);
        let i = result_in(&e, 2);
        let n = e.fz.chunks[i].len();
        assert!(n > 2, "{n} chunks");
        let whole: i64 = e.fz.chunks[i].iter().map(|c| c.tokens).sum();
        assert_eq!(e.sends_now(i, None), whole);
        // all but the last chunk already left: only that one is still paid for on every call
        e.drop_as(i, 0..n - 1, "evict");
        assert_eq!(e.sends_now(i, None), e.fz.chunks[i][n - 1].tokens);
        // one line in a hundred is sent by a copy that stays: leaving would not save it
        let one: Vec<bool> = (0..100).map(|k| k == 0).collect();
        let (last, tokens) = (e.fz.chunks[i][n - 1].tokens, e.tr.units[i].tokens);
        assert!(last > tokens / 100, "{last} {tokens}");
        assert_eq!(e.sends_now(i, Some(&one)), last - tokens / 100);
    }

    #[test]
    fn live_with_named_an_output_of_the_file_the_request_names_stays() {
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1;named=16000";
        // the request names listing.txt: its reads are the material the answer needs, used or not yet
        let e = &mut reads(spec, &[(1, 40), (41, 80), (81, 120)]);
        assert_eq!(output(&e.view(), 2), listing(1, 40));
        // a request that names another file lets the unused read leave, as without `named`
        let mut e = Engine::new(params(spec).unwrap());
        let mut msgs = vec![json!({"role": "user", "content": "Read notes.md and say what slot 50 holds."})];
        for (k, (a, b)) in [(1, 40), (41, 80), (81, 120)].into_iter().enumerate() {
            msgs.extend(step(k + 1, &format!("sed -n '{a},{b}p' listing.txt"), &listing(a, b)));
        }
        e.sync(&msgs);
        assert!(output(&e.view(), 2).contains("left out here"));
    }

    #[test]
    fn lead_an_output_that_leaves_keeps_the_line_naming_what_nothing_followed_yet() {
        // the agent has looked up one handler by name; the read that leaves points at another
        let read = format!("{}\n 1200:  jmp handler_77", listing(1, 40));
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "grep -rn handler_12 src", "src/a.rs:3: handler_12()"));
        msgs.extend(step(2, "sed -n '1,41p' listing.txt", &read));
        msgs.extend(step(3, "sed -n '41,80p' listing.txt", &listing(41, 80)));
        msgs.extend(step(4, "sed -n '81,120p' listing.txt", &listing(81, 120)));
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
        for (lead, kept) in [("lead=0", false), ("lead=1", true)] {
            let mut e = Engine::new(params(&format!("{spec};{lead}")).unwrap());
            e.sync(&msgs);
            let out = output(&e.view(), 4);
            assert!(out.contains("left out here") && !out.contains("slot 1 "), "{out}");
            assert_eq!(out.ends_with("\n 1200:  jmp handler_77"), kept, "{lead}: {out}");
        }
    }

    #[test]
    fn brief_the_note_for_an_output_left_out_only_says_how_to_bring_it_back() {
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
        msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1;follow=1";
        let mut e = Engine::new(params(&format!("{spec};brief=1")).unwrap());
        e.sync(&msgs);
        assert_eq!(
            output(&e.view(), 2),
            "[curator] bash output left out here, 40 lines you read already; run it again to see it again"
        );
    }

    #[test]
    fn digest_what_the_agent_read_stays_until_it_writes_about_it() {
        // the agent reads two ranges and looks further without a word on what it read so far
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "sed -n '41,80p' listing.txt", &listing(41, 80)));
        msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
        for (digest, kept) in [("digest=0", false), ("digest=1", true)] {
            let mut e = Engine::new(params(&format!("{spec};{digest}")).unwrap());
            e.sync(&msgs);
            let view = e.view();
            assert_eq!(output(&view, 2) == listing(1, 40), kept, "{digest}: {view:?}");
        }
        // once it has written what it found, the read it did not draw on goes
        msgs.push(
            json!({"role": "assistant", "content": [{"type": "text", "text": "Finding: the slots run in order."}]}),
        );
        msgs.extend(step(4, "sed -n '121,160p' listing.txt", &listing(121, 160)));
        let mut e = Engine::new(params(&format!("{spec};digest=1")).unwrap());
        e.sync(&msgs);
        let view = e.view();
        assert_ne!(output(&view, 2), listing(1, 40), "{view:?}");
    }

    #[test]
    fn follow_a_call_that_printed_what_it_went_for_does_not_keep_the_output_that_named_it() {
        // the agent looks up an address the first read printed, and the lookup prints that line again
        let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
        msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
        msgs.extend(step(2, "grep -n 1052 listing.txt", &listing(7, 7)));
        msgs.extend(step(3, "sed -n '81,120p' listing.txt", &listing(81, 120)));
        let spec = "nudge=0;eoq=0;link=0;threshold=0;close=2;margin=0;ahead=1;live=1";
        for (follow, kept) in [("follow=0", true), ("follow=1", false)] {
            let mut e = Engine::new(params(&format!("{spec};{follow}")).unwrap());
            e.sync(&msgs);
            let view = e.view();
            assert_eq!(output(&view, 2) == listing(1, 40), kept, "{follow}: {view:?}");
            assert_eq!(output(&view, 4), listing(7, 7));
        }
    }

    #[test]
    fn aim_a_request_brings_back_the_step_that_went_for_what_it_names_not_one_that_only_listed_it() {
        // the first read lists the folder and prints the sales file; the second goes for the stock file
        let mut msgs = vec![json!({"role": "user", "content": "Summarize the files in data/."})];
        msgs.extend(step(1, "ls data; cat data/sales.csv", &format!("sales.csv\nstock.csv\n{}", report())));
        msgs.extend(step(2, "head -3 data/stock.csv", "item,count\nbolt_7,40\nnut_9,12"));
        msgs.extend(step(4, "wc -l data/sales.csv", "31 data/sales.csv"));
        msgs.extend(step(5, "du -sh data", "8K data"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Sales and stock."}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
        msgs.extend(step(3, "wc -l notes.md", "12 notes.md"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": "What does stock.csv hold?"}));
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        for (aim, sales) in [("aim=0", true), ("aim=1", false)] {
            let mut e = Engine::new(params(&format!("{spec};{aim}")).unwrap());
            // the reads leave once the next request is open, and the last one asks about them again
            e.sync(&msgs[..msgs.len() - 4]);
            assert!(!sent(&e.view()).contains("bolt_7"));
            e.sync(&msgs);
            let view = e.view();
            let last = sent(&view[view.len() - 1..]);
            assert!(last.contains("bolt_7"), "{aim}: {last}");
            assert_eq!(last.contains(&word(0)), sales, "{aim}: {last}");
        }
    }

    #[test]
    fn a_request_on_a_topic_brings_back_the_step_that_says_it_most_not_the_largest_that_names_it() {
        // the catalog names the pricing once in its header; the notes are about it
        let catalog = format!("shop catalog, pricing in euros\n{}", report());
        let notes = "pricing changed in march\nnew pricing for spring\nold pricing ends in may";
        let mut msgs = vec![json!({"role": "user", "content": "Summarize the shop files."})];
        msgs.extend(step(1, "cat catalog.txt", &catalog));
        msgs.extend(step(2, "cat notes.txt", notes));
        msgs.extend(step(4, "du -sh .", "8K ."));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "A catalog and notes."}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has todo.md?"}));
        msgs.extend(step(3, "wc -l todo.md", "12 todo.md"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": "What about pricing?"}));
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;aim=1").unwrap());
        e.sync(&msgs[..msgs.len() - 4]);
        assert!(!sent(&e.view()).contains("spring"));
        e.sync(&msgs);
        let view = e.view();
        let last = sent(&view[view.len() - 1..]);
        assert!(last.contains("spring"), "{last}");
        assert!(!last.contains(&word(0)), "{last}");
    }

    #[test]
    fn named_a_read_stays_while_the_latest_request_names_its_file() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000";
        // the next request is about the report: what was read of it stays
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions("Done.", Some("Now sort the report by date, please.")));
        assert!(sent(&e.view()).contains(&word(0)[..]));
        // the next request is about something else: the read leaves, as without `named`
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
        assert!(!sent(&e.view()).contains(&word(0)[..]));
    }

    /// A request that names a file again after the talk moved away: what comes back of it.
    fn named_again(reads: &[(usize, &str, String)], edit_after: Option<usize>) -> String {
        let mut msgs =
            vec![json!({"role": "user", "content": "Which client has the largest invoice total in report.txt?"})];
        for (k, command, out) in reads {
            msgs.extend(step(*k, command, out));
            if edit_after == Some(*k) {
                msgs.extend(step(900, "sed -i 's/client/customer/' report.txt", ""));
            }
        }
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
        msgs.push(json!({"role": "user", "content": "How many lines has notes.md?"}));
        msgs.extend(step(901, "wc -l notes.md", "12 notes.md"));
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
        msgs.push(json!({"role": "user", "content": "Now sort report.txt by date, please."}));
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000").unwrap());
        e.sync(&msgs);
        sent(&e.view())
    }

    #[test]
    fn named_a_read_an_edit_made_out_of_date_does_not_come_back() {
        let old = (0..30).map(|k| format!("row {} client {}", word(k), word(k + 700))).collect::<Vec<_>>().join("\n");
        let new = (0..30).map(|k| format!("row {} customer {}", word(k), word(k + 900))).collect::<Vec<_>>().join("\n");
        let view = named_again(&[(1, "cat report.txt", old), (2, "cat report.txt", new)], Some(1));
        assert!(view.contains(&word(900)[..]), "the read after the edit comes back");
        assert!(!view.contains(&word(700)[..]), "the read before the edit stays out: {view}");
    }

    #[test]
    fn named_what_comes_back_of_a_file_read_in_many_parts_is_bounded() {
        // 40 parts of 400 lines: far more than the request can use at once
        let reads: Vec<(usize, String, String)> = (0..40)
            .map(|k| {
                (k + 1, format!("sed -n '{},{}p' report.txt", 400 * k, 400 * k + 399), listing(400 * k, 400 * k + 399))
            })
            .collect();
        let reads: Vec<(usize, &str, String)> = reads.iter().map(|(k, c, o)| (*k, c.as_str(), o.clone())).collect();
        let view = named_again(&reads, None);
        let whole: usize = reads.iter().map(|r| r.2.len()).sum();
        assert!(view.len() < whole / 2, "brought back {} of {} bytes", view.len(), whole);
        // the newest part is among what came back
        assert!(view.contains(&listing(400 * 39, 400 * 39 + 2)[..20]), "the newest part comes back");
    }

    #[test]
    fn mission_a_read_of_a_file_the_first_message_lists_stays_until_a_request_takes_it() {
        let ledger = "quokka 41\nwombat 17\nnumbat 23";
        let session = |last: &str| {
            let mut msgs = vec![json!({"role": "user", "content": "The material is report.txt and ledger.csv. \
                Which client has the largest invoice total in report.txt?"})];
            msgs.extend(step(1, "cat report.txt", &report()));
            msgs.extend(step(2, "cat ledger.csv", ledger));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
            msgs.push(json!({"role": "user", "content": "Now count the lines of report.txt."}));
            msgs.extend(step(3, "wc -l report.txt", "12 report.txt"));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "12"}]}));
            msgs.push(json!({"role": "user", "content": last}));
            msgs
        };
        let base = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1;named=16000";
        let kept = |spec: &str, last: &str| {
            let mut e = Engine::new(params(spec).unwrap());
            e.sync(&session(last));
            sent(&e.view()).contains("quokka 41")
        };
        // the list is being worked through (the report had its request), and no request has been about the
        // ledger yet: what was read of it stays, it is still to come
        assert!(kept(&format!("{base};mission=1"), "Rename notes.md to todo.md, please."));
        assert!(!kept(base, "Rename notes.md to todo.md, please."));
        // the ledger had its request: after it, the read leaves as any other
        let mut msgs = session("Sum the second column of ledger.csv.");
        msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "81"}]}));
        msgs.push(json!({"role": "user", "content": "Rename notes.md to todo.md, please."}));
        let mut e = Engine::new(params(&format!("{base};mission=1")).unwrap());
        e.sync(&msgs);
        assert!(!sent(&e.view()).contains("quokka 41"));
    }

    #[test]
    fn a_line_the_host_adds_after_the_results_is_part_of_the_same_request() {
        // the model gets the results and the line as one message, so the answer closes the read
        let mut msgs = sessions("Done.", None);
        msgs.insert(3, json!({"role": "user", "content": "Give your answer now, please."}));
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1").unwrap());
        e.sync(&msgs);
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]), "{view}");
        assert!(view.contains("are left out of this context: bash cat report.txt"), "{view}");
    }

    #[test]
    fn a_history_that_leaves_at_once_keeps_its_newest_lines_in_the_pool_in_order() {
        // 24 requests of 500 lines read each leave together: more lines than the pool holds
        let mut msgs = Vec::new();
        for k in 0..24 {
            msgs.push(json!({"role": "user", "content": format!("Read part {k} of listing.txt.")}));
            msgs.extend(step(
                k,
                &format!("sed -n '{},{}p' listing.txt", 500 * k, 500 * k + 499),
                &listing(500 * k, 500 * k + 499),
            ));
            msgs.push(json!({"role": "assistant", "content": listing(500 * k, 500 * k + 499)}));
        }
        msgs.push(json!({"role": "user", "content": "Say which slot holds the last word."}));
        let mut e = Engine::new(params("nudge=0;ledger=6000;protect=2").unwrap());
        e.sync(&msgs);
        let sizes = vec![1000; msgs.len()];
        assert!(e.leave(&sizes, 1.0));
        assert!(e.start > 4 * 24 - 8, "start {}", e.start);
        assert_eq!(e.pool.len(), POOL);
        assert!(e.pool.windows(2).all(|w| w[0].0 <= w[1].0), "the pool keeps the order of the messages");
        // the newest output that left is in the pool whole, the oldest is not
        let last = (0..e.start).rev().find(|&mi| output_of(&msgs, mi).is_some()).unwrap();
        let newest = output_of(&msgs, last).unwrap();
        let first_line = crate::py::squash_ws(newest.lines().next().unwrap());
        assert!(e.pool.iter().any(|(mi, ln, _, _)| *mi == last && *ln == first_line), "{first_line}");
        assert!(e.pool.iter().all(|(mi, _, _, _)| *mi > 3), "the first read is out of the pool");
    }

    /// The text of the tool result of message `mi` of `msgs`, if it holds one.
    fn output_of(msgs: &[Value], mi: usize) -> Option<String> {
        let b = blocks(msgs[mi].get("content")).into_owned();
        let r = b.iter().find(|x| get_str(x, "type") == "tool_result")?;
        Some(text_of(r.get("content")).into_owned())
    }

    #[test]
    fn a_request_about_something_else_leaves_the_read_out() {
        let spec = "nudge=0;eoq=0;link=0;close=2;margin=0;ahead=1";
        let mut e = Engine::new(params(spec).unwrap());
        e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
        let view = sent(&e.view());
        assert!(!view.contains(&word(0)[..]), "{view}");
        assert!(!view.contains("Brought back"), "{view}");
    }

    #[test]
    fn by_use_a_bare_reply_lets_no_step_go() {
        // "[4]" shows nothing of where it came from, so nothing of what the request read may go
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=1;margin=0;ahead=1").unwrap());
        e.sync(&sessions("[4]", None));
        assert!(sent(&e.view()).contains(&word(0)[..]));
        // a request done from an output of its own shows the read was not needed
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=1;margin=0;ahead=1").unwrap());
        e.sync(&sessions("Done.", Some("Rename notes.md to todo.md, please.")));
        assert!(!sent(&e.view()).contains(&word(0)[..]));
    }

    #[test]
    fn a_step_waits_until_keeping_it_has_cost_more_than_its_leaving() {
        // nothing has paid for the cut yet: with no calls to come counted, the read stays
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2;ahead=0").unwrap());
        e.sync(&sessions("Done.", None));
        assert!(sent(&e.view()).contains(&word(0)[..]));
        assert!(e.steps.iter().all(|s| !s.out && s.idle.is_some()));
    }

    #[test]
    fn a_name_every_request_says_still_points_at_the_one_step_that_holds_it() {
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2").unwrap());
        let mut msgs = Vec::new();
        let reads = ["cat ledger.csv", "cat notes.txt", "cat todo.txt"];
        for (k, q) in ["In ledger.csv: the invoices?", "In ledger.csv: the clients?", "In ledger.csv: the totals?"]
            .iter()
            .enumerate()
        {
            msgs.push(json!({"role": "user", "content": q}));
            msgs.extend(step(k + 1, reads[k], &format!("line {k}\n")));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
        }
        msgs.pop();
        e.sync(&msgs);
        e.view();
        let asked: Vec<String> = e.asks(2).iter().map(|&w| e.int.get(w).to_string()).collect();
        assert!(asked.iter().any(|w| w.starts_with("ledger")), "{asked:?}");
    }

    #[test]
    fn what_every_request_says_points_at_no_step() {
        let mut e = Engine::new(params("nudge=0;eoq=0;link=0;close=2").unwrap());
        let mut msgs = Vec::new();
        for (k, q) in
            ["Project Zephyr: list the invoices.", "Project Zephyr: list the clients.", "Project Zephyr: the totals?"]
                .iter()
                .enumerate()
        {
            msgs.push(json!({"role": "user", "content": q}));
            msgs.extend(step(k + 1, "cat report.txt", &report()));
            msgs.push(json!({"role": "assistant", "content": [{"type": "text", "text": "Done."}]}));
        }
        msgs.pop();
        e.sync(&msgs);
        e.view();
        let asked: Vec<String> = e.asks(2).iter().map(|&w| e.int.get(w).to_string()).collect();
        assert!(asked.contains(&"totals".to_string()), "{asked:?}");
        assert!(!asked.contains(&"zephyr".to_string()) && !asked.contains(&"project".to_string()), "{asked:?}");
    }

    #[test]
    fn topics_keep_long_words_and_labels() {
        let mut int = Interner::default();
        let got: Vec<String> = {
            let mut v: Vec<String> = topics("Function B returns boost::any; see assembly-B.txt, 2024 a I", &mut int)
                .into_iter()
                .map(|w| int.get(w).to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(got, ["B", "I", "any", "assembly", "boost", "function", "returns", "see", "txt"]);
    }

    /// A step of the agent with a tool of its own: the call and its output (`error`: the tool failed).
    fn tool(k: usize, name: &str, input: Value, out: &str, error: bool) -> [Value; 2] {
        let id = format!("call_{k}");
        [
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": out, "is_error": error}]}),
        ]
    }

    /// The text of a file of `n` lines, different for every `seed`.
    fn source(seed: usize, n: usize) -> String {
        (0..n)
            .map(|k| format!("let v{k} = {} + {};", word(seed + k), word(seed + k + 500)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A session that reads `src/pricing.rs` and then `src/other.rs`, as big as each other, edits
    /// `src/pricing.rs` (the edit fails with `failed`), and goes on with short steps.
    fn edited_session(failed: bool) -> Vec<Value> {
        let mut msgs = vec![json!({"role": "user", "content": "Fix the rounding in the pricing code."})];
        msgs.extend(tool(1, "read_file", json!({"path": "src/pricing.rs"}), &source(0, 120), false));
        msgs.extend(tool(2, "read_file", json!({"path": "src/other.rs"}), &source(5000, 120), false));
        let edit = json!({"path": "src/pricing.rs", "old_str": "v1", "new_str": "w1"});
        msgs.extend(tool(3, "edit_file", edit, if failed { "no match" } else { "ok" }, failed));
        for k in 4..12 {
            msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
        }
        msgs
    }

    /// The read result in message `mi`.
    fn result_in(e: &Engine, mi: usize) -> usize {
        *e.fz.by_msg(mi).iter().find(|&&i| e.tr.units[i].kind == UKind::Result).unwrap()
    }

    #[test]
    fn a_read_an_edit_of_its_file_made_out_of_date_is_marked_and_the_view_stays_until_a_cut() {
        let msgs = edited_session(false);
        let mut on = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
        on.sync(&msgs);
        assert_eq!(on.superseded_by(result_in(&on, 2)), Some(("src/pricing.rs".to_string(), 5)));
        // another file's read is not out of date
        assert_eq!(on.superseded_by(result_in(&on, 4)), None);
        // with no cut the view is the one sent with the setting off, and the same on each request
        let mut off = Engine::new(params("stale=0;eoq=0;threshold=0").unwrap());
        off.sync(&msgs);
        let first = on.view();
        assert_eq!(first, off.view());
        assert_eq!(first, on.view());
        let mut more = msgs.clone();
        more.extend(step(12, "wc -l part12", "12 lines"));
        on.sync(&more);
        assert_eq!(&on.view()[..first.len()], &first[..]);
    }

    #[test]
    fn past_the_cap_a_read_out_of_date_leaves_before_one_of_the_same_size_and_its_note_names_the_edit() {
        let msgs = edited_session(false);
        let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
        whole.sync(&msgs);
        let total = whole.tokens();
        let cap = total - 1;
        let floor = (total - est_str(&source(0, 120)) / 2) as f64 / cap as f64;
        for cut in ["age", "need"] {
            for recall in [false, true] {
                let given = format!("stale=1;threshold=0;nudge=0;brief=1;cap={cap};floor={floor};cut={cut}");
                let mut e = Engine::new(params(&given).unwrap());
                if recall {
                    e.tools(&[RECALL_TOOL.to_string()]);
                }
                e.sync(&msgs);
                let view = e.view();
                assert_eq!(output(&view, 4), source(5000, 120), "{cut}: the read of the other file stays");
                let back = if recall {
                    "recall m00003 brings it back as it was"
                } else {
                    "read the file again to see it as it is now"
                };
                assert_eq!(
                    output(&view, 2),
                    format!(
                        "[curator] read_file output left out here: the read of src/pricing.rs from before its edit in m00006; {back}"
                    ),
                    "{cut}"
                );
            }
        }
        // with the setting off, the cut by age takes the oldest messages, the other file's read with them
        let mut off = Engine::new(params(&format!("stale=0;threshold=0;nudge=0;cap={cap};floor={floor}")).unwrap());
        off.sync(&msgs);
        assert!(!sent(&off.view()).contains(&source(5000, 120)));
    }

    #[test]
    fn a_failed_edit_a_later_read_and_other_files_leave_a_read_as_it_was_and_a_part_read_goes_out_of_date() {
        let e = |msgs: &[Value]| {
            let mut e = Engine::new(params("stale=1;eoq=0").unwrap());
            e.sync(msgs);
            e
        };
        // a failed edit did not change the file
        let failed = e(&edited_session(true));
        assert_eq!(failed.superseded_by(result_in(&failed, 2)), None);
        // a read after the edit holds the file as it is
        let mut msgs = edited_session(false);
        msgs.extend(tool(20, "read_file", json!({"path": "./src/pricing.rs"}), &source(9000, 120), false));
        let after = e(&msgs);
        let last = msgs.len() - 1;
        assert_eq!(after.superseded_by(result_in(&after, last)), None);
        assert!(after.superseded_by(result_in(&after, 2)).is_some());
        // a part of the file, and a write by a shell command, or one under a longer path
        let mut msgs = vec![json!({"role": "user", "content": "Fix it."})];
        msgs.extend(tool(
            1,
            "read_file",
            json!({"path": "src/pricing.rs", "offset": 10, "limit": 20}),
            &source(0, 20),
            false,
        ));
        msgs.extend(tool(2, "read_file", json!({"path": "lib/pricing.rs"}), &source(100, 20), false));
        msgs.extend(step(3, "sed -i 's/v1/w1/' src/pricing.rs", "done"));
        assert!(same_file("/work/src/pricing.rs", "src/pricing.rs") && !same_file("src/pricing.rs", "lib/pricing.rs"));
        let part = e(&msgs);
        assert_eq!(part.superseded_by(result_in(&part, 2)), Some(("src/pricing.rs".to_string(), 5)));
        // a file of the same name elsewhere is another file
        assert_eq!(part.superseded_by(result_in(&part, 4)), None);
        // a chat without tools has nothing to mark
        let chat = e(&[json!({"role": "user", "content": "Hi"}), json!({"role": "assistant", "content": "Hello"})]);
        assert!(chat.edits.is_empty() && chat.stale_reads(&[0, 1]).is_empty());
    }

    #[test]
    fn a_failed_partial_edit_leaves_the_written_file_and_its_pin_as_they_were() {
        let body = source(0, 30);
        for failed in [true, false] {
            let mut msgs = vec![json!({"role": "user", "content": "Set up config.json and read src/main.rs."})];
            msgs.extend(tool(1, "read_file", json!({"path": "src/main.rs"}), &source(400, 30), false));
            msgs.extend(tool(2, "write_file", json!({"path": "config.json", "content": body}), "ok", false));
            // an edit as big as the whole file, and a small one, of each file
            let big = json!({"path": "config.json", "old_str": body, "new_str": "x"});
            msgs.extend(tool(3, "edit_file", big, if failed { "no match" } else { "ok" }, failed));
            let small = json!({"path": "src/main.rs", "old_str": "v1", "new_str": "w1"});
            msgs.extend(tool(4, "edit_file", small, if failed { "no match" } else { "ok" }, failed));
            let mut e = Engine::new(params("pin=4000").unwrap());
            e.sync(&msgs);
            assert_eq!(e.wrote.contains_key("config.json"), failed, "failed={failed}");
            assert_eq!(e.written.contains_key("src/main.rs"), !failed, "failed={failed}");
            assert!(e.written.contains_key("config.json"), "failed={failed}");
        }
    }

    #[test]
    fn windows_paths_are_paths() {
        assert!(same_file(r"C:\proj\src\pricing.rs", r"src\pricing.rs"));
        assert!(same_file(r"C:\proj\src\pricing.rs", "src/pricing.rs"));
        assert!(same_file(r".\src\pricing.rs", "src/pricing.rs"));
        assert!(!same_file(r"C:\proj\src\pricing.rs", r"lib\pricing.rs"));
        assert_eq!(basename(r"C:\proj\src\pricing.rs"), "pricing.rs");
        assert_eq!(file_key(r".\src\\b\index.ts"), "src/b/index.ts");
        for name in
            [r"src\_try.rs", r"C:\Users\me\AppData\Local\Temp\out.json", r"%TEMP%\run\x.csv", r"C:\Windows\Temp\a.txt"]
        {
            assert!(scratch(name), "{name}");
        }
        assert!(!scratch(r"C:\proj\src\main.rs"));
        let parts = |v: &[&[&str]]| -> Vec<Vec<String>> {
            v.iter().map(|p| p.iter().map(|s| s.to_string()).collect()).collect()
        };
        assert_eq!(files_in(r"Read C:\proj\src\calc_util.rs"), parts(&[&["calc", "util"]]));
        assert_eq!(files_in(r"type src\calc.rs ..\lib\db.rs"), parts(&[&["calc"], &["db"]]));
        assert_eq!(files_in(r"Read C:/proj/src/calc.rs"), parts(&[&["calc"]]));
        // a pattern with escapes is no path
        assert!(files_in(r"grep -n 'foo\.txt' a\|b \d+\.rs").is_empty());
        // a changed file named in a failure, by its stem, on Windows too
        let failures = vec!["FAILED tests/test_calc.py::test_add".to_string()];
        assert!(named_in(r"C:\proj\src\calc.py", &failures));
    }

    #[test]
    fn underscore_framework_files_are_not_scratch() {
        for name in [
            "_app.tsx",
            "pages/_document.tsx",
            "pkg/__init__.py",
            "__main__.py",
            "styles/_variables.scss",
            "_config.yml",
            "src/tmpl/page.html",
        ] {
            assert!(!scratch(name), "{name}");
        }
        for name in
            ["tmp_out.txt", "_tmp.rs", "_try.rs", "_scratch.md", "scratch.txt", "/tmp/out.json", "/var/tmp/run/x.csv"]
        {
            assert!(scratch(name), "{name}");
        }
        // over the budget, a framework file is kept like any other and the scratch file goes first
        let mut msgs = vec![json!({"role": "user", "content": "Set up the app shell."})];
        msgs.extend(tool(1, "write_file", json!({"path": "pages/_app.tsx", "content": source(0, 40)}), "ok", false));
        msgs.extend(tool(2, "write_file", json!({"path": "_try.rs", "content": source(300, 40)}), "ok", false));
        msgs.extend(tool(3, "write_file", json!({"path": "src/main.rs", "content": source(600, 40)}), "ok", false));
        let mut e = Engine::new(params("pin=4000").unwrap());
        e.sync(&msgs);
        e.p.pin = est_str(&e.wrote_text(msgs.len())) - 1;
        e.harvest(0, msgs.len());
        let kept: Vec<&str> = e.wrote.values().map(|w| w.path.as_str()).collect();
        assert!(kept.contains(&"pages/_app.tsx") && !kept.contains(&"_try.rs"), "{kept:?}");
    }

    #[test]
    fn same_basename_in_two_dirs_keeps_both_writes_and_pins() {
        let mut msgs = vec![json!({"role": "user", "content": "Compare src/a/index.ts with src/b/index.ts."})];
        msgs.extend(tool(1, "read_file", json!({"path": "src/a/index.ts"}), &source(0, 10), false));
        msgs.extend(tool(2, "read_file", json!({"path": "src/b/index.ts"}), &source(100, 10), false));
        msgs.extend(tool(3, "write_file", json!({"path": "src/a/main.ts", "content": source(200, 10)}), "ok", false));
        msgs.extend(tool(4, "write_file", json!({"path": "src/b/main.ts", "content": source(300, 10)}), "ok", false));
        let edit = json!({"path": "src/b/main.ts", "old_str": "v1", "new_str": "w1"});
        msgs.extend(tool(5, "edit_file", edit, "ok", false));
        let mut e = Engine::new(params("pin=4000").unwrap());
        e.sync(&msgs);
        e.harvest(0, msgs.len());
        // both writes stay, and the edit marks only its own file
        let wrote: Vec<(&str, Option<usize>)> = e.wrote.values().map(|w| (w.path.as_str(), w.edited)).collect();
        assert_eq!(wrote, vec![("src/a/main.ts", None), ("src/b/main.ts", Some(9))]);
        // both reads are pinned
        let mut pinned: Vec<&str> = e.pins.values().map(|p| p.1.as_str()).collect();
        pinned.sort();
        assert_eq!(pinned, vec!["src/a/index.ts", "src/b/index.ts"]);
        assert_eq!(file_key(".//src//b/index.ts"), "src/b/index.ts");
        // a later write of one takes its pin and leaves the other's
        msgs.extend(step(6, "sed -i 's/v1/w1/' src/b/index.ts", "done"));
        e.sync(&msgs);
        e.harvest(0, msgs.len());
        let pinned: Vec<&str> = e.pins.values().map(|p| p.1.as_str()).collect();
        assert_eq!(pinned, vec!["src/a/index.ts"]);
        // a write named by the file name alone reaches every file of that name
        msgs.extend(step(7, "cd src && sed -i 's/v2/w2/' index.ts", "done"));
        msgs.extend(step(8, "cd src && sed -i 's/v3/w3/' main.ts", "done"));
        e.sync(&msgs);
        e.harvest(0, msgs.len());
        assert!(e.pins.is_empty(), "{:?}", e.pins);
        assert!(e.wrote.values().all(|w| w.edited.is_some()), "{:?}", e.wrote);
    }

    #[test]
    fn failed_whole_write_keeps_the_earlier_version() {
        let mut msgs = vec![json!({"role": "user", "content": "Write the rates module."})];
        msgs.extend(tool(1, "write_file", json!({"path": "src/rates.rs", "content": source(0, 10)}), "ok", false));
        let mut e = Engine::new(params("pin=4000").unwrap());
        e.sync(&msgs);
        // the next write is sent, and its result says it failed
        let [call, result] = tool(
            2,
            "write_file",
            json!({"path": "src/rates.rs", "content": source(50, 10)}),
            "permission denied",
            true,
        );
        msgs.push(call);
        e.sync(&msgs);
        msgs.push(result);
        e.sync(&msgs);
        let kept: Vec<(&str, usize, &str)> =
            e.wrote.values().map(|w| (w.path.as_str(), w.mi, w.text.as_str())).collect();
        assert_eq!(kept, vec![("src/rates.rs", 1, source(0, 10).as_str())]);
        // its lines are not taken for those of a later version
        let first = *e.fz.by_msg(1).iter().find(|&&i| e.tr.units[i].kind == UKind::Call).unwrap();
        assert!(e.superseded(first, 1).is_empty());
        // a write that went through replaces it
        msgs.extend(tool(3, "write_file", json!({"path": "src/rates.rs", "content": source(90, 10)}), "ok", false));
        e.sync(&msgs);
        assert_eq!(e.wrote.values().map(|w| w.mi).collect::<Vec<_>>(), vec![5]);
    }

    #[test]
    fn a_rule_given_again_in_the_message_that_comes_with_a_rewrite_moves_past_what_withdrew_it() {
        let user = |t: &str| json!({"role": "user", "content": t});
        let reply = |t: &str| json!({"role": "assistant", "content": t});
        let texts = |e: &Engine| e.rules.list.iter().map(|r| r.text.clone()).collect::<Vec<_>>();
        let mut e = Engine::new(params("nudge=0").unwrap());
        let mut msgs = vec![user("Fix the login form."), reply("On it.")];
        msgs.extend([user("Never push to main."), reply("Understood.")]);
        msgs.extend([user("You may push to main from now on."), reply("Noted.")]);
        e.sync(&msgs);
        assert_eq!(texts(&e), ["Never push to main.", "You may push to main from now on."]);
        // the host compacts the history; the same request carries the person's new message
        let summary = "Summary: the person said never push to main, then allowed it.";
        e.sync(&[user(summary), reply("Continuing."), user("Never push to main.")]);
        assert_eq!(texts(&e), ["You may push to main from now on.", "Never push to main."]);
        // read anew, a rule before the last reply stays where it was
        let mut e = Engine::new(params("nudge=0").unwrap());
        e.sync(&[user("Never push to main."), reply("Ok."), user("You may push to main from now on.")]);
        e.sync(&[user("Never push to main."), reply("Ok."), user("Go on.")]);
        assert_eq!(texts(&e), ["Never push to main.", "You may push to main from now on."]);
    }

    #[test]
    fn a_rewrite_with_no_reply_is_read_anew_but_its_newest_message() {
        let user = |t: &str| json!({"role": "user", "content": t});
        let reply = |t: &str| json!({"role": "assistant", "content": t});
        let rules = |e: &Engine| e.rules.list.iter().map(|r| (r.text.clone(), r.turn)).collect::<Vec<_>>();
        let mut msgs = vec![user("Fix the login form."), reply("On it.")];
        msgs.extend([user("Never push to main."), reply("Understood.")]);
        msgs.extend([user("You may push to main from now on."), reply("Noted.")]);
        let fresh = || {
            let mut e = Engine::new(params("nudge=0").unwrap());
            e.sync(&msgs);
            e
        };
        let before = rules(&fresh());
        // the host compacts the history into one summary that quotes the rules in another order
        let summary = "Summary of the session so far.\nYou may push to main from now on.\nNever push to main.";
        let mut e = fresh();
        e.sync(&[user(summary)]);
        assert_eq!(rules(&e), before);
        // the summary and a new message, no reply between them: only the new message is new
        let mut e = fresh();
        e.sync(&[user(summary), user("Go on.")]);
        assert_eq!(rules(&e), before);
        let mut e = fresh();
        e.sync(&[user(summary), user("Never push to main.")]);
        let texts: Vec<String> = rules(&e).into_iter().map(|r| r.0).collect();
        assert_eq!(texts, ["You may push to main from now on.", "Never push to main."]);
    }
}
